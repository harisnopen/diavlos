//! The local socket: how commands, the MCP server, the web UI and the
//! bindings talk to the helper. One JSON line in, one out; streaming ops
//! keep writing lines until the caller goes away. 0600 on Unix.

use std::sync::Arc;
use std::time::Duration;

use diavlos_client::proto::{
    AskResult, CheckApproveResult, DraftWire, ExportResult, HelloResult, InviteResult, JoinResult,
    OutboxItem, ReadResult, Request, Response, RoomStatus, RotateResult, SendResult, StatusResult,
    WhoEntry, WhoamiResult,
};
use diavlos_client::Paths;
use diavlos_core::{
    message::{now_ts, APPROVE_TTL_SECS},
    names::validate_name,
    room::new_room_id,
    secrets, ControlOp, Error, InviteSpec, Kind, LocalMember, Member, Message, MessageType, Policy,
    Result, Role, Room,
};
use interprocess::local_socket::tokio::{prelude::*, Listener, Stream};
use interprocess::local_socket::ListenerOptions;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{info, warn};

use super::outbox::Outcome;
use super::Helper;
use crate::net::Wire;

/// How long `send` waits for the home before answering "queued".
const SEND_WAIT: Duration = Duration::from_secs(3);

pub fn bind(paths: &Paths) -> anyhow::Result<Listener> {
    #[cfg(unix)]
    {
        use interprocess::os::unix::local_socket::ListenerOptionsExt;
        // interprocess applies the mode with fchmod() before bind(). Linux
        // allows that on a socket; macOS refuses it with EINVAL, which comes
        // back as Unsupported. Fall back to the umask way there.
        let opts = ListenerOptions::new()
            .name(paths.socket_name()?)
            .mode(0o600);
        match opts.create_tokio() {
            Ok(listener) => Ok(listener),
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => bind_with_umask(paths),
            Err(e) => Err(e.into()),
        }
    }
    #[cfg(not(unix))]
    {
        let opts = ListenerOptions::new().name(paths.socket_name()?);
        Ok(opts.create_tokio()?)
    }
}

/// Bind with the umask narrowed so the socket file is born owner-only,
/// then set it to 0600. For Unix systems where the mode cannot be set on
/// the socket before bind (macOS).
#[cfg(unix)]
fn bind_with_umask(paths: &Paths) -> anyhow::Result<Listener> {
    use std::os::unix::fs::PermissionsExt;
    let name = paths.socket_name()?;
    // SAFETY: umask(2) only reads and sets this process's file mode mask.
    // The helper is the only thing running in this process, and the old
    // mask is put back right after the bind.
    let old = unsafe { libc::umask(0o077) };
    let result = ListenerOptions::new().name(name).create_tokio();
    // SAFETY: as above.
    unsafe { libc::umask(old) };
    let listener = result?;
    std::fs::set_permissions(paths.socket_file(), std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub async fn serve(helper: Arc<Helper>, listener: Arc<Listener>) {
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

async fn write_line(stream: &Stream, resp: &Response) -> std::io::Result<()> {
    let mut out = serde_json::to_string(resp).unwrap_or_default();
    out.push('\n');
    let mut writer = stream;
    writer.write_all(out.as_bytes()).await?;
    writer.flush().await
}

async fn handle(helper: Arc<Helper>, stream: Stream) -> anyhow::Result<()> {
    let mut line = String::new();
    {
        let mut reader = BufReader::new(&stream);
        if reader.read_line(&mut line).await? == 0 {
            return Ok(());
        }
    }
    let req = match serde_json::from_str::<Request>(line.trim_end()) {
        Ok(req) => req,
        Err(e) => {
            write_line(
                &stream,
                &Response::err(&Error::Invalid(format!("bad request: {e}"))),
            )
            .await?;
            return Ok(());
        }
    };
    match req {
        Request::Events { follow } => {
            let r = stream_events(&helper, &stream, follow).await;
            if let Err(e) = r {
                let _ = write_line(&stream, &Response::err(&e)).await;
            }
            Ok(())
        }
        Request::Watch { room, identity } => {
            let r = stream_watch(&helper, &stream, &room, &identity).await;
            if let Err(e) = r {
                let _ = write_line(&stream, &Response::err(&e)).await;
            }
            Ok(())
        }
        req => {
            let stop = matches!(req, Request::Stop);
            let resp = match dispatch(&helper, req).await {
                Ok(v) => Response::ok(v),
                Err(e) => Response::err(&e),
            };
            if stop {
                helper.request_shutdown();
            }
            write_line(&stream, &resp).await?;
            Ok(())
        }
    }
}

async fn dispatch(helper: &Arc<Helper>, req: Request) -> Result<serde_json::Value> {
    let v = match req {
        Request::Hello => serde_json::to_value(HelloResult {
            version: diavlos_core::VERSION.into(),
            node: helper.net.node_id(),
        })?,
        Request::Whoami { identity } => {
            let id = helper.identity(&identity).await?;
            let key = id.public();
            serde_json::to_value(WhoamiResult {
                label: identity,
                name: id.name.clone(),
                kind: id.kind,
                key: key.to_string(),
                fingerprint: key.fingerprint(),
            })?
        }
        Request::Status => serde_json::to_value(status(helper).await?)?,
        Request::Stop => serde_json::json!({"stopping": true}),
        Request::Rooms => serde_json::to_value(status(helper).await?.rooms)?,
        Request::NewRoom {
            name,
            about,
            identity,
            retention_days,
            class,
        } => serde_json::to_value(
            new_room(helper, &name, &about, &identity, retention_days, class).await?,
        )?,
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
        Request::Ask {
            room,
            identity,
            draft,
            timeout_secs,
        } => serde_json::to_value(ask(helper, &room, &identity, draft, timeout_secs).await?)?,
        Request::Who { room } => serde_json::to_value(who(helper, &room).await?)?,
        Request::Claim {
            room,
            identity,
            task_id,
        } => serde_json::to_value(
            send(
                helper,
                &room,
                &identity,
                DraftWire {
                    text: "claimed".into(),
                    kind: Some(MessageType::Claim),
                    reply_to: Some(task_id),
                    ..Default::default()
                },
            )
            .await?,
        )?,
        Request::Release {
            room,
            identity,
            task_id,
        } => serde_json::to_value(
            send(
                helper,
                &room,
                &identity,
                DraftWire {
                    text: "released".into(),
                    kind: Some(MessageType::Release),
                    reply_to: Some(task_id),
                    ..Default::default()
                },
            )
            .await?,
        )?,
        Request::Control {
            room,
            identity,
            control: op,
        } => serde_json::to_value(control(helper, &room, &identity, op).await?)?,
        Request::Deny {
            room,
            identity,
            msg_id,
            reason,
        } => serde_json::to_value(
            send(
                helper,
                &room,
                &identity,
                DraftWire {
                    text: reason,
                    kind: Some(MessageType::Deny),
                    reply_to: Some(msg_id),
                    ..Default::default()
                },
            )
            .await?,
        )?,
        Request::Approve {
            room,
            identity,
            msg_id,
        } => serde_json::to_value(
            send(
                helper,
                &room,
                &identity,
                DraftWire {
                    text: "approved".into(),
                    kind: Some(MessageType::Approve),
                    reply_to: Some(msg_id),
                    ..Default::default()
                },
            )
            .await?,
        )?,
        Request::CheckApprove { room, action } => {
            serde_json::to_value(check_approve(helper, &room, &action).await?)?
        }
        Request::Export {
            room,
            identity,
            since,
        } => serde_json::to_value(export(helper, &room, &identity, since.as_deref()).await?)?,
        Request::Rotate { room, identity } => {
            serde_json::to_value(rotate(helper, &room, &identity).await?)?
        }
        Request::Outbox { room } => serde_json::to_value(outbox_list(helper, room.as_deref())?)?,
        Request::OutboxRetry { id } => serde_json::to_value(outbox_retry(helper, &id).await?)?,
        Request::OutboxDrop { id } => serde_json::to_value(outbox_drop(helper, &id)?)?,
        Request::Events { .. } | Request::Watch { .. } => {
            return Err(Error::Invalid("streaming op on a plain call".into()))
        }
    };
    Ok(v)
}

async fn status(helper: &Helper) -> Result<StatusResult> {
    let mut rooms = Vec::new();
    let home_links = helper.home_links.lock().await;
    for room in helper.store.list_rooms()? {
        let home = helper.is_home(&room);
        let outbox = helper.store.outbox_counts(&room.id)?;
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
            queued: outbox.queued(),
            failed: outbox.failed,
            quarantined: outbox.quarantined,
            me: helper
                .store
                .local_members(&room.id)?
                .into_iter()
                .map(|lm| lm.member.name)
                .collect(),
            paused: room.paused || room.closed,
        });
    }
    Ok(StatusResult {
        version: diavlos_core::VERSION.into(),
        node: helper.net.node_id(),
        home_dir: helper.paths.home.display().to_string(),
        rooms,
        network: helper.net.network_info(),
        encrypted_inbox: helper.store.is_encrypted(),
        metrics_addr: helper.metrics_addr.clone(),
    })
}

fn outbox_item(helper: &Helper, e: diavlos_core::OutboxEntry) -> OutboxItem {
    let room = helper
        .store
        .room_by_id(&e.room_id)
        .ok()
        .flatten()
        .map(|r| r.name)
        .unwrap_or_else(|| e.room_id.clone());
    let text = e
        .message
        .as_ref()
        .map(|m| {
            let line = m.text.lines().next().unwrap_or("");
            let mut t: String = line.chars().take(60).collect();
            if t.len() < line.len() || m.text.lines().nth(1).is_some() {
                t.push('…');
            }
            t
        })
        .unwrap_or_default();
    OutboxItem {
        id: e.msg_id,
        room,
        sender: e.sender,
        state: if e.unreadable {
            format!("{} (unreadable)", e.state.as_str())
        } else {
            e.state.as_str().to_string()
        },
        kind: e.message.as_ref().map(|m| m.kind),
        text,
        created: e.created,
        attempts: e.attempts,
        retry_at: e.retry_at,
        reason: e.reason,
        reason_class: e.reason_class,
        code: e.reason_code,
    }
}

fn outbox_list(helper: &Helper, room: Option<&str>) -> Result<Vec<OutboxItem>> {
    let room_id = match room {
        Some(r) => Some(helper.store.room(r)?.id),
        None => None,
    };
    Ok(helper
        .store
        .outbox_entries(room_id.as_deref())?
        .into_iter()
        .map(|e| outbox_item(helper, e))
        .collect())
}

async fn outbox_retry(helper: &Helper, id: &str) -> Result<OutboxItem> {
    if !helper.store.outbox_retry(id, &helper.now_ts())? {
        return Err(Error::Invalid(format!(
            "{id} is not a failed or quarantined message in the outbox"
        )));
    }
    let e = helper
        .store
        .outbox_get(id)?
        .ok_or_else(|| Error::Invalid(format!("{id} vanished")))?;
    helper.emit(
        "send_retried",
        Some(&e.room_id),
        serde_json::json!({"id": id, "sender": e.sender}),
    );
    helper.wake_room(&e.room_id).await;
    Ok(outbox_item(helper, e))
}

fn outbox_drop(helper: &Helper, id: &str) -> Result<OutboxItem> {
    if !helper.store.outbox_drop(id, &helper.now_ts())? {
        return Err(Error::Invalid(format!(
            "{id} is not a failed or quarantined message in the outbox; only those can be dropped"
        )));
    }
    let e = helper
        .store
        .outbox_get(id)?
        .ok_or_else(|| Error::Invalid(format!("{id} vanished")))?;
    helper.emit(
        "send_dropped",
        Some(&e.room_id),
        serde_json::json!({"id": id, "sender": e.sender}),
    );
    Ok(outbox_item(helper, e))
}

fn local_member(helper: &Helper, room: &Room, identity: &str) -> Result<LocalMember> {
    helper
        .store
        .local_member(&room.id, identity)?
        .ok_or_else(|| Error::NotInRoom(room.name.clone()))
}

pub(super) async fn new_room(
    helper: &Helper,
    name: &str,
    about: &str,
    identity: &str,
    retention_days: Option<u32>,
    class: Option<diavlos_core::DataClass>,
) -> Result<Room> {
    validate_name(name)?;
    let id = helper.identity(identity).await?;
    let now = now_ts();
    let room = Room {
        id: new_room_id(),
        name: name.to_string(),
        about: about.to_string(),
        owner: id.public(),
        created: now.clone(),
        retention_days,
        class: class.unwrap_or_default(),
        paused: false,
        hold: false,
        closed: false,
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
    let mut msg = Message::new(draft, id.as_ref())?;
    helper.store.sequence_and_append(&mut msg)?;
    info!(room = %room.id, name = %room.name, "room made");
    helper.emit(
        "room_made",
        Some(&room.id),
        serde_json::json!({"name": room.name}),
    );
    Ok(room)
}

pub(super) async fn invite(
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
    if room.closed {
        return Err(Error::Denied(
            "that room was rotated; invite into the new one".into(),
        ));
    }
    validate_name(name)?;
    if let Some(m) = helper.store.member_by_name(&room.id, name)? {
        if !m.revoked {
            return Err(Error::NameTaken(name.to_string()));
        }
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
    helper.emit(
        "invite_made",
        Some(&room.id),
        serde_json::json!({"name": name, "role": role, "human": human, "pinned": inv.for_node.is_some()}),
    );
    Ok(InviteResult {
        invite: inv.encode(),
        name: name.to_string(),
        role,
        expires: inv.expires,
        for_node: inv.for_node,
    })
}

pub(super) async fn join(helper: &Arc<Helper>, token: &str, identity: &str) -> Result<JoinResult> {
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
    if inv.home_node == helper.net.node_id() {
        // The room lives on this very helper: another key on the same
        // machine is joining. No network needed.
        let me = helper.net.node_id();
        let (room, members, _) =
            super::peers::admit(helper, None, &me, token.trim(), id.signed_profile()).await?;
        let mine = members
            .iter()
            .find(|m| m.key == id.public())
            .cloned()
            .ok_or_else(|| Error::Denied("home did not list us as a member".into()))?;
        helper.store.upsert_member(&mine, Some(identity))?;
        helper.emit(
            "room_joined",
            Some(&room.id),
            serde_json::json!({"name": mine.name, "room": room.name, "local": true}),
        );
        return Ok(JoinResult {
            name: mine.name,
            members,
            messages: helper.store.message_count(&room.id)?,
            room,
        });
    }
    let link = helper.net.dial(&inv.home_node, &inv.home_hints).await?;
    let joined = join_over(helper, &link, token, identity).await;
    link.close();
    joined
}

/// Join over a link to the room's home that is already open.
pub(super) async fn join_over(
    helper: &Arc<Helper>,
    link: &Arc<dyn crate::net::Link>,
    token: &str,
    identity: &str,
) -> Result<JoinResult> {
    let id = helper.identity(identity).await?;
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
    helper.emit(
        "room_joined",
        Some(&room.id),
        serde_json::json!({"name": my_name, "room": room.name}),
    );
    helper.ensure_room_task(&room.id).await;
    Ok(JoinResult {
        name: my_name,
        members: helper.store.members(&room.id)?,
        messages: helper.store.message_count(&room.id)?,
        room,
    })
}

/// Turn a draft into a signed message, filling in what the type needs.
async fn build(
    helper: &Helper,
    room: &Room,
    lm: &LocalMember,
    identity: &str,
    mut draft: DraftWire,
) -> Result<Message> {
    let id = helper.identity(identity).await?;
    if helper.config.helper.secret_scan {
        if let Some(hit) = secrets::scan_message(
            &draft.text,
            &draft.data,
            draft.action.as_ref(),
            draft.trace.as_deref(),
        ) {
            helper.emit(
                "secret_refused",
                Some(&room.id),
                serde_json::json!({"kind": hit}),
            );
            return Err(Error::Denied(format!(
                "refused: that looks like a {hit}, so it was not sent. (secret_scan = false in config turns the scan off)"
            )));
        }
    }
    let mut extra = diavlos_core::Draft::default();
    if draft.kind == Some(MessageType::Approve) {
        let target_id = draft
            .reply_to
            .clone()
            .ok_or_else(|| Error::Invalid("an approve needs --reply-to <question id>".into()))?;
        let target = helper
            .store
            .message_by_id(&target_id)?
            .ok_or_else(|| Error::Invalid(format!("no message {target_id} here")))?;
        let action = target
            .action
            .as_ref()
            .ok_or_else(|| Error::Denied("that question carries no action to approve".into()))?;
        extra.action_hash = Some(action.hash());
        extra.expires = Some(
            (chrono::Utc::now() + chrono::Duration::seconds(APPROVE_TTL_SECS))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        extra.once = Some(true);
        if draft.text.is_empty() {
            draft.text = "approved".into();
        }
    }
    helper.draft_from(room, &lm.member, &id, draft, extra)
}

pub(super) async fn send(
    helper: &Helper,
    room: &str,
    identity: &str,
    draft: DraftWire,
) -> Result<SendResult> {
    let room = helper.store.room(room)?;
    let lm = local_member(helper, &room, identity)?;
    if room.closed {
        return Err(Error::Denied(format!(
            "room {} was rotated; ask the owner for a new invite",
            room.name
        )));
    }
    let msg = build(helper, &room, &lm, identity, draft).await?;
    if helper.is_home(&room) {
        let msg = helper.sequence_here(&room, msg, None).await?;
        return Ok(SendResult {
            message: msg,
            delivered: true,
        });
    }
    // The home decides. When it is out of reach, say no now to what it
    // would say no to anyway, from what this helper last heard, so the
    // sender is told rather than left with a queued message that will be
    // dropped later.
    if helper.home_link(&room.id).await.is_none() {
        if room.paused {
            return Err(Error::RoomPaused(room.name.clone()));
        }
        lm.member
            .check_may_send(msg.kind, lm.member.key == room.owner, &now_ts())?;
        helper.check_typed(&room, &msg)?;
    }
    // On disk before send returns.
    helper.store.outbox_add(&msg)?;
    helper.wake_room(&room.id).await;
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
            if let Some(done) = helper.store.message_by_id(&msg.id)? {
                return Ok(Outcome::Sequenced(Box::new(done)));
            }
            Ok::<_, Error>(super::outbox::submit(helper, &room, &link, &msg).await)
        })
        .await;
        match outcome {
            Ok(Ok(Outcome::Sequenced(sequenced))) => {
                return Ok(SendResult {
                    message: *sequenced,
                    delivered: true,
                })
            }
            // Not reached, or no answer in time: it stays queued and goes
            // when the home is back.
            Ok(Ok(Outcome::Temporary {
                class: "transport" | "local",
                ..
            }))
            | Err(_) => {}
            // The sender is right here: tell them now and do not queue.
            Ok(Ok(Outcome::Temporary { error, .. }))
            | Ok(Ok(Outcome::Definitive(error)))
            | Ok(Ok(Outcome::Unknown(error)))
            | Ok(Err(error)) => {
                helper.store.outbox_remove(&msg.id)?;
                return Err(error);
            }
        }
    }
    Ok(SendResult {
        message: msg,
        delivered: false,
    })
}

/// Wait until a room has a message the caller wants, or the deadline.
async fn wait_for<F>(
    helper: &Helper,
    room: &Room,
    deadline: Option<tokio::time::Instant>,
    mut check: F,
) -> Result<Message>
where
    F: FnMut() -> Result<Option<Message>>,
{
    let mut rx = helper.subscribe();
    loop {
        if let Some(m) = check()? {
            return Ok(m);
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
                    // One last look before giving up.
                    if let Some(m) = check()? {
                        return Ok(m);
                    }
                    return Err(Error::TimedOut);
                }
            }
            None => wait.await,
        }
    }
}

fn deadline(timeout_secs: u64) -> Option<tokio::time::Instant> {
    if timeout_secs == 0 {
        None
    } else {
        Some(tokio::time::Instant::now() + Duration::from_secs(timeout_secs))
    }
}

/// Messages an agent wants to wake up for: not its own, not helper or
/// owner housekeeping.
fn wanted(m: &Message, me: &str) -> bool {
    m.from != me && !matches!(m.kind, MessageType::System | MessageType::Control)
}

/// Wait for the next message from someone else. Skips your own and
/// housekeeping, moving the bookmark past them.
pub(super) async fn next(
    helper: &Helper,
    room: &str,
    identity: &str,
    timeout_secs: u64,
) -> Result<Message> {
    let room = helper.store.room(room)?;
    let lm = local_member(helper, &room, identity)?;
    let reader = lm.member.name.clone();
    let dl = deadline(timeout_secs);
    wait_for(helper, &room, dl, || {
        let bookmark = helper.store.bookmark(&room.id, &reader)?;
        let batch = helper.store.messages_after(&room.id, bookmark, 100)?;
        let mut skip_to = bookmark;
        for m in batch {
            if wanted(&m, &reader) {
                helper.store.set_bookmark(&room.id, &reader, m.seq)?;
                return Ok(Some(m));
            }
            skip_to = m.seq;
        }
        if skip_to > bookmark {
            helper.store.set_bookmark(&room.id, &reader, skip_to)?;
        }
        Ok(None)
    })
    .await
}

/// Read from your bookmark onward. Never deletes. Moves the bookmark to
/// the last message returned. A read from a given `since` is a look back:
/// it leaves the bookmark where it is, so a web page or a script paging
/// through the log never makes an agent miss or repeat a message.
pub(super) async fn read(
    helper: &Helper,
    room: &str,
    identity: &str,
    since: Option<u64>,
    limit: u32,
) -> Result<ReadResult> {
    let room = helper.store.room(room)?;
    let lm = local_member(helper, &room, identity)?;
    let reader = lm.member.name.clone();
    let start = match since {
        Some(s) => s.saturating_sub(1),
        None => helper.store.bookmark(&room.id, &reader)?,
    };
    let messages = helper
        .store
        .messages_after(&room.id, start, limit.clamp(1, 1000))?;
    if since.is_none() {
        if let Some(last) = messages.last() {
            helper.store.set_bookmark(&room.id, &reader, last.seq)?;
        }
    }
    Ok(ReadResult {
        bookmark: helper.store.bookmark(&room.id, &reader)?,
        messages,
    })
}

/// Send a question and wait for a reply to that exact message. A deny is
/// a reply too; the caller sees its type.
async fn ask(
    helper: &Helper,
    room: &str,
    identity: &str,
    mut draft: DraftWire,
    timeout_secs: u64,
) -> Result<AskResult> {
    draft.kind = Some(MessageType::Question);
    let sent = send(helper, room, identity, draft).await?;
    let question = sent.message;
    let room = helper.store.room(room)?;
    let me = local_member(helper, &room, identity)?.member.name;
    let dl = deadline(timeout_secs);
    let qid = question.id.clone();
    // A question that carries an action is asking for permission: only a
    // human-signed approve or deny answers it. Other replies are just talk.
    let needs_human = question.action.is_some();
    let reply = wait_for(helper, &room, dl, || {
        Ok(helper
            .store
            .replies_to(&room.id, &qid)?
            .into_iter()
            .find(|m| {
                m.from != me
                    && (!needs_human || matches!(m.kind, MessageType::Approve | MessageType::Deny))
            }))
    })
    .await?;
    Ok(AskResult { question, reply })
}

async fn who(helper: &Helper, room: &str) -> Result<Vec<WhoEntry>> {
    let room = helper.store.room(room)?;
    let now = now_ts();
    let mut out = Vec::new();
    let me = helper.net.node_id();
    for m in helper.store.members(&room.id)? {
        let online = match &m.node {
            Some(n) if *n == me => true,
            Some(n) if helper.is_home(&room) => helper.node_online(&room.id, n).await,
            Some(_) => m
                .last_seen
                .as_ref()
                .map(|s| {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .map(|t| {
                            (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds() < 120
                        })
                        .unwrap_or(false)
                })
                .unwrap_or(false),
            None => false,
        };
        let profile = m
            .profile
            .get("profile")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        out.push(WhoEntry {
            name: m.name.clone(),
            kind: m.kind.to_string(),
            role: m.role.to_string(),
            fingerprint: m.key.fingerprint(),
            key: m.key.to_string(),
            profile,
            last_seen: m.last_seen.clone(),
            expires_at: m.expires_at.clone(),
            owner: m.key == room.owner,
            muted: m.muted,
            revoked: m.revoked,
            expired: m.is_expired(&now),
            node: m.node.clone(),
            online,
        });
    }
    Ok(out)
}

#[cfg(test)]
pub(super) async fn control_for_test(helper: &Helper, room: &str, identity: &str, op: ControlOp) {
    control(helper, room, identity, op).await.unwrap();
}

/// Owner ops. Sent as a control message; applied once sequenced.
async fn control(helper: &Helper, room: &str, identity: &str, op: ControlOp) -> Result<SendResult> {
    let r = helper.store.room(room)?;
    let id = helper.identity(identity).await?;
    if id.public() != r.owner {
        return Err(Error::Denied("only the room owner can do that".into()));
    }
    if let ControlOp::Grant { name, .. }
    | ControlOp::Mute { name }
    | ControlOp::Unmute { name }
    | ControlOp::Revoke { name } = &op
    {
        let m = helper
            .store
            .member_by_name(&r.id, name)?
            .ok_or_else(|| Error::Invalid(format!("no member named {name}")))?;
        if m.key == r.owner {
            return Err(Error::Denied("the owner cannot do that to itself".into()));
        }
    }
    let draft = DraftWire {
        text: op.describe(),
        kind: Some(MessageType::Control),
        data: serde_json::to_value(&op)?,
        ..Default::default()
    };
    send(helper, room, identity, draft).await
}

/// Exit 0 if a valid, unexpired, unused human approve exists for exactly
/// this action. Spends it: one approve, one deed.
pub(super) async fn check_approve(
    helper: &Helper,
    room: &str,
    action: &diavlos_core::Action,
) -> Result<CheckApproveResult> {
    let room = helper.store.room(room)?;
    let hash = action.hash();
    let now = now_ts();
    for a in helper.store.approvals_for(&room.id, &hash)? {
        let Some(exp) = &a.expires else { continue };
        if exp.as_str() <= now.as_str() {
            continue;
        }
        // SPEC 2.5 check 5: an approve dated in the future does not count.
        if a.ts.as_str() > now.as_str() {
            continue;
        }
        let Some(m) = helper.store.member_by_name(&room.id, &a.from)? else {
            continue;
        };
        // Checks 2 and 3, as they stand now rather than when it was sent:
        // a human key that still holds the approver role (or owns the
        // room), not revoked, muted or expired since.
        if m.kind != Kind::Human
            || m.check_may_send(MessageType::Approve, m.key == room.owner, &now)
                .is_err()
        {
            continue;
        }
        if a.verify(&m.key).is_err() {
            continue;
        }
        if helper.store.approval_use(&a.id, &hash, &now)? {
            helper.emit(
                "approve_used",
                Some(&room.id),
                serde_json::json!({"approve": a.id, "by": a.from, "action_hash": hash}),
            );
            return Ok(CheckApproveResult {
                approve_id: a.id.clone(),
                approved_by: a.from.clone(),
                action_hash: hash,
                expires: exp.clone(),
            });
        }
    }
    Err(Error::Denied(
        "no valid, unexpired, unused human approve for exactly this action".into(),
    ))
}

async fn export(
    helper: &Helper,
    room: &str,
    identity: &str,
    since: Option<&str>,
) -> Result<ExportResult> {
    let room = helper.store.room(room)?;
    let lm = local_member(helper, &room, identity)?;
    let id = helper.identity(identity).await?;
    let messages = match since {
        Some(s) => {
            let ts = if s.len() == 10 {
                format!("{s}T00:00:00Z")
            } else {
                s.to_string()
            };
            helper.store.messages_from_ts(&room.id, &ts, u32::MAX)?
        }
        None => helper.store.messages_after(&room.id, 0, u32::MAX)?,
    };
    let members = helper.store.members(&room.id)?;
    let bundle = diavlos_core::bundle::export(
        &room,
        &members,
        &messages,
        &lm.member.name,
        id.as_ref(),
        since,
    )?;
    helper.emit(
        "export",
        Some(&room.id),
        serde_json::json!({"by": lm.member.name, "count": messages.len()}),
    );
    Ok(ExportResult {
        count: messages.len() as u64,
        bundle,
    })
}

/// New room key. Everyone out. The old room stays for audit, closed.
async fn rotate(helper: &Arc<Helper>, room: &str, identity: &str) -> Result<RotateResult> {
    let old = helper.store.room(room)?;
    let id = helper.identity(identity).await?;
    if id.public() != old.owner || !helper.is_home(&old) {
        return Err(Error::Denied(
            "only the room owner, where the room lives, can rotate it".into(),
        ));
    }
    if old.closed {
        return Err(Error::Denied("that room is already closed".into()));
    }
    let lm = local_member(helper, &old, identity)?;
    let new_id = new_room_id();
    // Tell everyone the old room is over.
    let op = ControlOp::Rotated {
        new_room_id: new_id.clone(),
    };
    let draft = diavlos_core::Draft {
        room: old.id.clone(),
        from: lm.member.name.clone(),
        kind: Some(MessageType::Control),
        text: op.describe(),
        data: serde_json::to_value(&op)?,
        ..Default::default()
    };
    let msg = Message::new(draft, id.as_ref())?;
    helper.sequence_here(&old, msg, None).await?;
    // Close and rename the old room; drop its links.
    let stamp = now_ts().replace([':', '-'], "");
    let old_name = format!("{}-rotated-{}", old.name, &stamp[..15.min(stamp.len())]);
    let mut closed = old.clone();
    closed.closed = true;
    closed.paused = true;
    helper.store.update_room(&closed)?;
    helper.store.rename_room(&old.id, &old_name)?;
    if let Some(links) = helper.links.lock().await.remove(&old.id) {
        for (_, l) in links {
            l.close();
        }
    }
    // Same name, new key, owner only.
    let room = new_room(
        helper,
        &old.name,
        &old.about,
        identity,
        old.retention_days,
        Some(old.class),
    )
    .await?;
    helper.emit(
        "rotated",
        Some(&old.id),
        serde_json::json!({"new_room": room.id, "name": room.name}),
    );
    Ok(RotateResult {
        old_room_id: old.id,
        old_room_name: old_name,
        room,
    })
}

async fn stream_events(helper: &Helper, stream: &Stream, follow: bool) -> Result<()> {
    let mut rx = helper.subscribe_events();
    for ev in helper.recent_events() {
        write_line(stream, &Response::ok(ev)).await?;
    }
    if !follow {
        return Ok(());
    }
    loop {
        match rx.recv().await {
            Ok(ev) => write_line(stream, &Response::ok(ev)).await?,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => return Ok(()),
        }
    }
}

/// Stream messages from the bookmark onward, then live. Moves the
/// bookmark as it goes.
async fn stream_watch(helper: &Helper, stream: &Stream, room: &str, identity: &str) -> Result<()> {
    let room = helper.store.room(room)?;
    let lm = local_member(helper, &room, identity)?;
    let reader = lm.member.name.clone();
    loop {
        let m = wait_for(helper, &room, None, || {
            let bookmark = helper.store.bookmark(&room.id, &reader)?;
            let batch = helper.store.messages_after(&room.id, bookmark, 100)?;
            let mut skip_to = bookmark;
            for m in batch {
                if wanted(&m, &reader) {
                    helper.store.set_bookmark(&room.id, &reader, m.seq)?;
                    return Ok(Some(m));
                }
                skip_to = m.seq;
            }
            if skip_to > bookmark {
                helper.store.set_bookmark(&room.id, &reader, skip_to)?;
            }
            Ok(None)
        })
        .await?;
        let (from_key, from_fingerprint) = match helper.store.member_by_name(&room.id, &m.from)? {
            Some(member) => (member.key.to_string(), member.key.fingerprint()),
            None => (String::new(), String::new()),
        };
        write_line(
            stream,
            &Response::ok(serde_json::json!({
                "message": m,
                "from_key": from_key,
                "from_fingerprint": from_fingerprint,
            })),
        )
        .await?;
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn umask_bind_makes_an_owner_only_socket() {
        let dir = std::env::temp_dir().join(format!(
            "diavlos-umask-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths { home: dir.clone() };
        let listener = bind_with_umask(&paths).unwrap();
        let mode = std::fs::metadata(paths.socket_file())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        // A client can still connect to it.
        let stream = Stream::connect(paths.socket_name().unwrap()).await;
        assert!(stream.is_ok());
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
