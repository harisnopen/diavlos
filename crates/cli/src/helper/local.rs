//! The local socket: how commands and the MCP server talk to the helper.
//! One JSON line in, one out. 0600 on Unix: only your user can open it.

use std::sync::Arc;
use std::time::Duration;

use diavlos_core::{
    message::now_ts, names::validate_name, room::new_room_id, Error, InviteSpec, Kind, Member,
    MessageType, Policy, Result, Role, Room,
};
use interprocess::local_socket::tokio::{prelude::*, Listener, Stream};
use interprocess::local_socket::ListenerOptions;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{info, warn};

use super::Helper;
use crate::net::Wire;
use crate::paths::Paths;
use crate::proto::{
    DraftWire, HelloResult, InviteResult, JoinResult, ReadResult, Request, Response, RoomStatus,
    SendResult, StatusResult,
};

/// How long `send` waits for the home before answering "queued".
const SEND_WAIT: Duration = Duration::from_secs(3);

pub fn bind(paths: &Paths) -> anyhow::Result<Listener> {
    let name = paths.socket_name()?;
    let opts = ListenerOptions::new().name(name);
    #[cfg(unix)]
    let opts = {
        use interprocess::os::unix::local_socket::ListenerOptionsExt;
        opts.mode(0o600)
    };
    Ok(opts.create_tokio()?)
}

pub async fn serve(helper: Arc<Helper>, listener: Listener) {
    loop {
        match listener.accept().await {
            Ok(stream) => {
                let h = helper.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(h, stream).await {
                        warn!(error = %e, "local request failed");
                    }
                });
            }
            Err(e) => {
                warn!(error = %e, "local accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn handle(helper: Arc<Helper>, stream: Stream) -> anyhow::Result<()> {
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        return Ok(());
    }
    let resp = match serde_json::from_str::<Request>(line.trim_end()) {
        Ok(req) => {
            let stop = matches!(req, Request::Stop);
            let resp = match dispatch(&helper, req).await {
                Ok(v) => Response::ok(v),
                Err(e) => Response::err(&e),
            };
            if stop {
                helper.request_shutdown();
            }
            resp
        }
        Err(e) => Response::err(&Error::Invalid(format!("bad request: {e}"))),
    };
    let mut out = serde_json::to_string(&resp)?;
    out.push('\n');
    let mut writer = &stream;
    writer.write_all(out.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

async fn dispatch(helper: &Arc<Helper>, req: Request) -> Result<serde_json::Value> {
    let v = match req {
        Request::Hello => serde_json::to_value(HelloResult {
            version: diavlos_core::VERSION.into(),
            node: helper.net.node_id(),
        })?,
        Request::Status => serde_json::to_value(status(helper).await?)?,
        Request::Stop => serde_json::json!({"stopping": true}),
        Request::Rooms => serde_json::to_value(status(helper).await?.rooms)?,
        Request::NewRoom {
            name,
            about,
            identity,
        } => serde_json::to_value(new_room(helper, &name, &about, &identity).await?)?,
        Request::Invite {
            room,
            name,
            human,
            for_node,
            role,
            identity,
        } => serde_json::to_value(
            invite(helper, &room, &name, human, for_node, role, &identity).await?,
        )?,
        Request::Join { invite, identity } => {
            serde_json::to_value(join(helper, &invite, &identity).await?)?
        }
        Request::Send {
            room,
            identity,
            draft,
        } => serde_json::to_value(send(helper, &room, &identity, draft).await?)?,
        Request::Next {
            room,
            identity,
            timeout_secs,
        } => serde_json::to_value(next(helper, &room, &identity, timeout_secs).await?)?,
        Request::Read {
            room,
            identity,
            since,
            limit,
        } => serde_json::to_value(read(helper, &room, &identity, since, limit).await?)?,
    };
    Ok(v)
}

async fn status(helper: &Helper) -> Result<StatusResult> {
    let mut rooms = Vec::new();
    let home_links = helper.home_links.lock().await;
    for room in helper.store.list_rooms()? {
        let home = helper.is_home(&room);
        rooms.push(RoomStatus {
            name: room.name.clone(),
            id: room.id.clone(),
            home,
            connected: home
                || home_links
                    .get(&room.id)
                    .map(|l| !l.is_closed())
                    .unwrap_or(false),
            members: helper.store.members(&room.id)?.len(),
            messages: helper.store.message_count(&room.id)?,
            queued: helper.store.outbox_count(&room.id)?,
            me: helper
                .store
                .local_members(&room.id)?
                .into_iter()
                .map(|lm| lm.member.name)
                .collect(),
            paused: room.paused,
        });
    }
    Ok(StatusResult {
        version: diavlos_core::VERSION.into(),
        node: helper.net.node_id(),
        home_dir: helper.paths.home.display().to_string(),
        rooms,
    })
}

async fn new_room(helper: &Helper, name: &str, about: &str, identity: &str) -> Result<Room> {
    validate_name(name)?;
    let id = helper.identity(identity).await?;
    let now = now_ts();
    let room = Room {
        id: new_room_id(),
        name: name.to_string(),
        about: about.to_string(),
        owner: id.public(),
        created: now.clone(),
        retention_days: None,
        class: Default::default(),
        paused: false,
        hold: false,
        home_node: helper.net.node_id(),
        home_hints: helper.net.hints(),
    };
    helper.store.create_room(&room)?;
    let me = Member {
        room_id: room.id.clone(),
        name: id.name.clone(),
        key: id.public(),
        kind: id.kind,
        role: Role::Approver,
        node: Some(helper.net.node_id()),
        granted_by: id.public(),
        expires_at: None,
        joined_at: now.clone(),
        last_seen: Some(now),
        muted: false,
        revoked: false,
        profile: serde_json::to_value(id.signed_profile()).unwrap_or_default(),
    };
    helper.store.upsert_member(&me, Some(identity))?;
    let policy_path = helper.paths.policy(name);
    let _ = Policy::default().save(&policy_path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(dir) = policy_path.parent() {
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            if let Some(rooms) = dir.parent() {
                let _ = std::fs::set_permissions(rooms, std::fs::Permissions::from_mode(0o700));
            }
        }
    }
    let draft = diavlos_core::Draft {
        room: room.id.clone(),
        from: me.name.clone(),
        kind: Some(MessageType::System),
        text: format!("{} made the room and joined as owner", me.name),
        data: serde_json::json!({"event": "joined", "member": me}),
        ..Default::default()
    };
    let mut msg = diavlos_core::Message::new(draft, id.as_ref())?;
    helper.store.sequence_and_append(&mut msg)?;
    info!(room = %room.id, name = %room.name, "room made");
    Ok(room)
}

async fn invite(
    helper: &Helper,
    room: &str,
    name: &str,
    human: bool,
    for_node: Option<String>,
    role: Option<Role>,
    identity: &str,
) -> Result<InviteResult> {
    let room = helper.store.room(room)?;
    let id = helper.identity(identity).await?;
    if id.public() != room.owner {
        return Err(Error::Denied("only the room owner can invite".into()));
    }
    if !helper.is_home(&room) {
        return Err(Error::Denied(
            "invites are made where the room lives".into(),
        ));
    }
    validate_name(name)?;
    if helper.store.member_by_name(&room.id, name)?.is_some() {
        return Err(Error::NameTaken(name.to_string()));
    }
    let role = role.unwrap_or(if human {
        Role::Approver
    } else {
        Role::TaskGiver
    });
    let inv = diavlos_core::Invite::create(
        InviteSpec {
            room_id: room.id.clone(),
            room_name: room.name.clone(),
            name: name.to_string(),
            kind: if human { Kind::Human } else { Kind::Agent },
            role,
            home_node: helper.net.node_id(),
            home_hints: helper.net.hints(),
            for_node,
            ttl_hours: None,
        },
        id.as_ref(),
    )?;
    helper.store.invite_record(&inv)?;
    info!(room = %room.id, name = %name, role = %role, pinned = inv.for_node.is_some(), "invite made");
    Ok(InviteResult {
        invite: inv.encode(),
        name: name.to_string(),
        role,
        expires: inv.expires,
        for_node: inv.for_node,
    })
}

async fn join(helper: &Arc<Helper>, token: &str, identity: &str) -> Result<JoinResult> {
    let inv = diavlos_core::Invite::decode(token)?;
    if let Some(pin) = &inv.for_node {
        if *pin != helper.net.node_id() {
            return Err(Error::Denied(
                "this invite is pinned to another machine".into(),
            ));
        }
    }
    let id = helper.identity(identity).await?;
    if let Some(existing) = helper.store.room_by_id(&inv.room_id)? {
        if let Some(lm) = helper.store.local_member(&existing.id, identity)? {
            return Ok(JoinResult {
                name: lm.member.name,
                members: helper.store.members(&existing.id)?,
                messages: helper.store.message_count(&existing.id)?,
                room: existing,
            });
        }
    }
    let link = helper.net.dial(&inv.home_node, &inv.home_hints).await?;
    let hello = Wire::Hello {
        v: diavlos_core::PROTOCOL_VERSION,
        node: helper.net.node_id(),
        version: diavlos_core::VERSION.into(),
    };
    link.request(&hello).await?.into_result()?;
    let req = Wire::Join {
        invite: token.trim().to_string(),
        profile: id.signed_profile(),
        hints: helper.net.hints(),
    };
    let reply = link.request(&req).await?.into_result()?;
    link.close();
    let Wire::JoinOk {
        room,
        members,
        messages,
    } = reply
    else {
        return Err(Error::Invalid(format!("unexpected {}", reply.label())));
    };
    let mut room = room;
    if helper.store.room_by_id(&room.id)?.is_none() {
        // Room names are unique per helper; if the name is taken by
        // another room, keep the id-based name.
        if helper.store.room_by_name(&room.name)?.is_some() {
            room.name = format!("{}-{}", room.name, &room.id[2..8]);
        }
        helper.store.create_room(&room)?;
    } else {
        helper.store.update_room(&room)?;
    }
    let mut my_name = None;
    for m in &members {
        let mine = m.key == id.public();
        if mine {
            my_name = Some(m.name.clone());
        }
        helper
            .store
            .upsert_member(m, if mine { Some(identity) } else { None })?;
    }
    let my_name =
        my_name.ok_or_else(|| Error::Denied("home did not list us as a member".into()))?;
    let (have, _) = helper.store.chain_head(&room.id)?;
    let fresh: Vec<_> = messages.into_iter().filter(|m| m.seq > have).collect();
    helper.apply_from_home(&room, &fresh)?;
    info!(room = %room.id, name = %my_name, "joined");
    tokio::spawn(super::peers::room_link_task(
        helper.clone(),
        room.id.clone(),
    ));
    Ok(JoinResult {
        name: my_name,
        members: helper.store.members(&room.id)?,
        messages: helper.store.message_count(&room.id)?,
        room,
    })
}

async fn send(helper: &Helper, room: &str, identity: &str, draft: DraftWire) -> Result<SendResult> {
    let room = helper.store.room(room)?;
    let lm = helper
        .store
        .local_member(&room.id, identity)?
        .ok_or_else(|| Error::NotInRoom(room.name.clone()))?;
    let id = helper.identity(identity).await?;
    let msg = helper.draft_from(&room, &lm.member, &id, draft)?;
    if helper.is_home(&room) {
        let msg = helper.sequence_here(&room, msg, None).await?;
        return Ok(SendResult {
            message: msg,
            delivered: true,
        });
    }
    // Say no now to what the home would say no to anyway, so the sender
    // is told even while the home is offline. The home checks again.
    if room.paused {
        return Err(Error::RoomPaused(room.name.clone()));
    }
    lm.member
        .check_may_send(msg.kind, lm.member.key == room.owner, &now_ts())?;
    // On disk before send returns.
    helper.store.outbox_add(&msg)?;
    helper.wake_room(&room.id).await;
    // Give the link a moment to come up (it may have just been made), then
    // submit ourselves so the sender learns the outcome right away.
    let deadline = tokio::time::Instant::now() + SEND_WAIT;
    let mut link = helper.home_link(&room.id).await;
    while link.is_none() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        link = helper.home_link(&room.id).await;
    }
    if let Some(link) = link {
        let lock = helper.submit_lock(&room.id).await;
        let outcome = tokio::time::timeout(SEND_WAIT, async {
            let _guard = lock.lock().await;
            if helper.store.message_by_id(&msg.id)?.is_some() {
                // The flush got there first.
                return helper
                    .store
                    .message_by_id(&msg.id)?
                    .ok_or_else(|| Error::Other("message vanished".into()));
            }
            super::peers::submit(helper, &room, &link, &msg).await
        })
        .await;
        match outcome {
            Ok(Ok(sequenced)) => {
                return Ok(SendResult {
                    message: sequenced,
                    delivered: true,
                })
            }
            Ok(Err(Error::ReachedNobody(_))) | Err(_) => {}
            Ok(Err(e)) => {
                // The home said no. The sender is told; it does not stay queued.
                helper.store.outbox_remove(&msg.id)?;
                return Err(e);
            }
        }
    }
    Ok(SendResult {
        message: msg,
        delivered: false,
    })
}

/// Wait for the next message from someone else. Skips your own and system
/// notices, moving the bookmark past them.
async fn next(
    helper: &Helper,
    room: &str,
    identity: &str,
    timeout_secs: u64,
) -> Result<diavlos_core::Message> {
    let room = helper.store.room(room)?;
    let lm = helper
        .store
        .local_member(&room.id, identity)?
        .ok_or_else(|| Error::NotInRoom(room.name.clone()))?;
    let reader = lm.member.name.clone();
    let mut rx = helper.subscribe();
    let deadline = if timeout_secs == 0 {
        None
    } else {
        Some(tokio::time::Instant::now() + Duration::from_secs(timeout_secs))
    };
    loop {
        let bookmark = helper.store.bookmark(&room.id, &reader)?;
        let batch = helper.store.messages_after(&room.id, bookmark, 100)?;
        let mut skip_to = bookmark;
        for m in batch {
            if m.from != reader && m.kind != MessageType::System {
                helper.store.set_bookmark(&room.id, &reader, m.seq)?;
                return Ok(m);
            }
            skip_to = m.seq;
        }
        if skip_to > bookmark {
            helper.store.set_bookmark(&room.id, &reader, skip_to)?;
        }
        let wait = async {
            loop {
                match rx.recv().await {
                    Ok((rid, _)) if rid == room.id => return,
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return,
                    Err(_) => {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        return;
                    }
                }
            }
        };
        match deadline {
            Some(d) => {
                if tokio::time::timeout_at(d, wait).await.is_err() {
                    return Err(Error::TimedOut);
                }
            }
            None => wait.await,
        }
    }
}

/// Read from your bookmark onward. Never deletes. Moves the bookmark to
/// the last message returned.
async fn read(
    helper: &Helper,
    room: &str,
    identity: &str,
    since: Option<u64>,
    limit: u32,
) -> Result<ReadResult> {
    let room = helper.store.room(room)?;
    let lm = helper
        .store
        .local_member(&room.id, identity)?
        .ok_or_else(|| Error::NotInRoom(room.name.clone()))?;
    let reader = lm.member.name.clone();
    let start = match since {
        Some(s) => s.saturating_sub(1),
        None => helper.store.bookmark(&room.id, &reader)?,
    };
    let messages = helper
        .store
        .messages_after(&room.id, start, limit.clamp(1, 1000))?;
    if let Some(last) = messages.last() {
        helper.store.set_bookmark(&room.id, &reader, last.seq)?;
    }
    Ok(ReadResult {
        bookmark: helper.store.bookmark(&room.id, &reader)?,
        messages,
    })
}
