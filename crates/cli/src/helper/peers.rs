//! Helper-to-helper traffic: accepting links, serving a room's members,
//! and keeping a link to the home of every room joined from elsewhere.

use std::sync::Arc;
use std::time::Duration;

use diavlos_core::{
    message::now_ts, Error, Invite, Member, Message, Result, Room, PROTOCOL_VERSION,
};
use tracing::{debug, info, warn};

use super::Helper;
use crate::net::{Link, Wire, SYNC_BATCH};

/// Take every link other helpers open to us.
pub async fn accept_loop(helper: Arc<Helper>) {
    while let Some(link) = helper.net.accept().await {
        let h = helper.clone();
        tokio::spawn(async move {
            let node = link.remote_node();
            debug!(node = %node, "link in");
            if let Err(e) = serve_link(h, link).await {
                debug!(node = %node, error = %e, "link out");
            }
        });
    }
}

/// Answer requests on one incoming link until it closes.
async fn serve_link(helper: Arc<Helper>, link: Arc<dyn Link>) -> Result<()> {
    let mut greeted = false;
    loop {
        let (req, reply) = link.next_request().await?;
        let label = req.label();
        let resp = if !greeted {
            match req {
                Wire::Hello { v, .. } if v == PROTOCOL_VERSION => {
                    greeted = true;
                    Wire::HelloOk {
                        v: PROTOCOL_VERSION,
                        version: diavlos_core::VERSION.into(),
                    }
                }
                Wire::Hello { v, .. } => Wire::Err {
                    code: 1,
                    msg: format!("protocol version {v} is not supported here (this helper speaks {PROTOCOL_VERSION})"),
                },
                _ => Wire::Err {
                    code: 1,
                    msg: "say hello first".into(),
                },
            }
        } else {
            match handle(&helper, &link, req).await {
                Ok(w) => w,
                Err(e) => Wire::error(&e),
            }
        };
        debug!(node = %link.remote_node(), req = label, resp = resp.label(), "served");
        reply.send(&resp).await?;
    }
}

async fn handle(helper: &Arc<Helper>, link: &Arc<dyn Link>, req: Wire) -> Result<Wire> {
    match req {
        Wire::Join {
            invite,
            profile,
            hints: _,
        } => handle_join(helper, link, &invite, profile).await,
        Wire::Submit { room_id, message } => {
            let room = helper
                .store
                .room_by_id(&room_id)?
                .ok_or(Error::NotInRoom(room_id))?;
            require_member_node(helper, &room, link)?;
            helper.register_link(&room.id, link.clone()).await?;
            let msg = helper
                .sequence_here(&room, message, Some(&link.remote_node()))
                .await?;
            Ok(Wire::Sequenced { message: msg })
        }
        Wire::Sync { room_id, have_seq } => {
            let room = helper
                .store
                .room_by_id(&room_id)?
                .ok_or(Error::NotInRoom(room_id))?;
            if !helper.is_home(&room) {
                return Err(Error::Denied("not the home of that room".into()));
            }
            let member = require_member_node(helper, &room, link)?;
            helper.register_link(&room.id, link.clone()).await?;
            helper.store.touch_member(
                &room.id,
                &member.name,
                Some(&link.remote_node()),
                &now_ts(),
            )?;
            let messages = helper
                .store
                .messages_after(&room.id, have_seq, SYNC_BATCH)?;
            let (head_seq, _) = helper.store.chain_head(&room.id)?;
            Ok(Wire::Messages {
                room_id: room.id.clone(),
                members: helper.store.members(&room.id)?,
                messages,
                head_seq,
            })
        }
        Wire::Push { room_id, messages } => {
            // A home pushing to us on a link it accepted from us is handled
            // in the room task; this covers a home that dialed us instead.
            let room = helper
                .store
                .room_by_id(&room_id)?
                .ok_or(Error::NotInRoom(room_id))?;
            if room.home_node != link.remote_node() {
                return Err(Error::Denied(
                    "pushes come from the room's home only".into(),
                ));
            }
            match helper.apply_from_home(&room, &messages) {
                Ok(seq) => Ok(Wire::Ack { seq }),
                Err(Error::Invalid(_)) => Ok(Wire::NeedSync { room_id: room.id }),
                Err(e) => Err(e),
            }
        }
        Wire::Hello { .. } => Ok(Wire::HelloOk {
            v: PROTOCOL_VERSION,
            version: diavlos_core::VERSION.into(),
        }),
        other => Err(Error::Invalid(format!("unexpected {}", other.label()))),
    }
}

/// The member bound to the node on the other end of this link.
fn require_member_node(helper: &Helper, room: &Room, link: &Arc<dyn Link>) -> Result<Member> {
    let node = link.remote_node();
    helper
        .store
        .members(&room.id)?
        .into_iter()
        .find(|m| m.node.as_deref() == Some(node.as_str()) && !m.revoked)
        .ok_or_else(|| Error::Denied("this machine is not a member of that room".into()))
}

/// Home side: let a joiner in with a signed invite.
async fn handle_join(
    helper: &Arc<Helper>,
    link: &Arc<dyn Link>,
    token: &str,
    profile: diavlos_core::SignedProfile,
) -> Result<Wire> {
    let inv = Invite::decode(token)?;
    let room = helper
        .store
        .room_by_id(&inv.room_id)?
        .ok_or_else(|| Error::Denied("no such room here".into()))?;
    if room.owner != inv.owner || !helper.is_home(&room) {
        return Err(Error::Denied(
            "invite was not issued by this room's owner".into(),
        ));
    }
    let (owner, owner_member) = helper
        .owner_identity(&room)
        .await?
        .ok_or_else(|| Error::Denied("this helper does not hold the owner key".into()))?;
    let now = now_ts();
    if inv.is_expired(&now) {
        return Err(Error::Denied("invite has expired".into()));
    }
    let remote = link.remote_node();
    if let Some(pin) = &inv.for_node {
        if *pin != remote {
            return Err(Error::Denied("invite is pinned to another machine".into()));
        }
    }
    profile.verify()?;
    let key = profile.key;
    let existing = helper.store.member_by_name(&room.id, &inv.name)?;
    let rejoin = matches!(&existing, Some(m) if m.key == key);
    if let Some(other) = helper.store.member_by_key(&room.id, &key)? {
        if other.name != inv.name {
            return Err(Error::Denied(format!(
                "this key is already in the room as {}",
                other.name
            )));
        }
    }
    if !rejoin {
        // One invite, one member, used once.
        if !helper
            .store
            .invite_use(&inv.nonce, &key.to_string(), &now)?
        {
            return Err(Error::Denied(
                "invite was already used, or was not issued here".into(),
            ));
        }
        if existing.is_some() {
            return Err(Error::NameTaken(inv.name.clone()));
        }
    }
    let member = Member {
        room_id: room.id.clone(),
        name: inv.name.clone(),
        key,
        kind: inv.kind,
        role: inv.role,
        node: Some(remote.clone()),
        granted_by: room.owner,
        expires_at: None,
        joined_at: existing
            .as_ref()
            .map(|m| m.joined_at.clone())
            .unwrap_or_else(|| now.clone()),
        last_seen: Some(now.clone()),
        muted: false,
        revoked: false,
        profile: serde_json::to_value(&profile).unwrap_or_default(),
    };
    helper.store.upsert_member(&member, None)?;
    helper.register_link(&room.id, link.clone()).await?;
    if !rejoin {
        let draft = diavlos_core::Draft {
            room: room.id.clone(),
            from: owner_member.name.clone(),
            kind: Some(diavlos_core::MessageType::System),
            text: format!(
                "{} joined as {} ({})",
                member.name, member.role, member.kind
            ),
            data: serde_json::json!({
                "event": "joined",
                "member": member,
            }),
            ..Default::default()
        };
        let mut msg = Message::new(draft, owner.as_ref())?;
        helper.store.sequence_and_append(&mut msg)?;
        helper.notify_room(&room.id, msg.seq);
        helper
            .push_to_members(&room, vec![msg], Some(&remote))
            .await;
        info!(room = %room.id, member = %member.name, node = %remote, "member joined");
    }
    let mut room_out = room.clone();
    room_out.home_hints = helper.net.hints();
    Ok(Wire::JoinOk {
        room: room_out,
        members: helper.store.members(&room.id)?,
        messages: helper.store.messages_after(&room.id, 0, u32::MAX)?,
    })
}

// ---- member side -----------------------------------------------------------

/// Keep a link to the home of a room we joined from elsewhere. Sync what we
/// missed, flush the outbox, take pushes. Retry forever with backoff.
pub async fn room_link_task(helper: Arc<Helper>, room_id: String) {
    let mut shutdown = helper.shutdown_signal();
    let waker = helper.waker(&room_id).await;
    let retry = Duration::from_secs(helper.config.helper.retry_secs.max(1));
    let mut backoff = retry;
    loop {
        if *shutdown.borrow() {
            return;
        }
        let Ok(Some(room)) = helper.store.room_by_id(&room_id) else {
            return;
        };
        match connect_home(&helper, &room).await {
            Ok(link) => {
                backoff = retry;
                helper
                    .home_links
                    .lock()
                    .await
                    .insert(room_id.clone(), link.clone());
                info!(room = %room.id, "linked to home");
                let outcome = serve_home_link(&helper, &room, &link, &waker, &mut shutdown).await;
                helper.home_links.lock().await.remove(&room_id);
                link.close();
                match outcome {
                    Ok(()) => return,
                    Err(e) => debug!(room = %room.id, error = %e, "home link ended"),
                }
            }
            Err(e) => {
                debug!(room = %room.id, error = %e, "home not reachable; will retry");
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = waker.notified() => {}
            _ = shutdown.changed() => return,
        }
    }
}

async fn connect_home(helper: &Helper, room: &Room) -> Result<Arc<dyn Link>> {
    let link = helper.net.dial(&room.home_node, &room.home_hints).await?;
    let hello = Wire::Hello {
        v: PROTOCOL_VERSION,
        node: helper.net.node_id(),
        version: diavlos_core::VERSION.into(),
    };
    match link.request(&hello).await?.into_result()? {
        Wire::HelloOk { .. } => Ok(link),
        other => Err(Error::Invalid(format!("unexpected {}", other.label()))),
    }
}

/// Returns `Ok(())` only on shutdown.
async fn serve_home_link(
    helper: &Arc<Helper>,
    room: &Room,
    link: &Arc<dyn Link>,
    waker: &Arc<tokio::sync::Notify>,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    sync_from_home(helper, room, link).await?;
    flush_outbox(helper, room, link).await?;
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    tick.tick().await;
    loop {
        tokio::select! {
            r = link.next_request() => {
                let (req, reply) = r?;
                let resp = match req {
                    Wire::Push { messages, .. } => match helper.apply_from_home(room, &messages) {
                        Ok(seq) => Wire::Ack { seq },
                        Err(Error::Invalid(_)) => Wire::NeedSync { room_id: room.id.clone() },
                        Err(e) => Wire::error(&e),
                    },
                    Wire::Hello { .. } => Wire::HelloOk { v: PROTOCOL_VERSION, version: diavlos_core::VERSION.into() },
                    other => Wire::Err { code: 1, msg: format!("unexpected {}", other.label()) },
                };
                let need_sync = matches!(resp, Wire::NeedSync { .. });
                reply.send(&resp).await?;
                if need_sync {
                    sync_from_home(helper, room, link).await?;
                }
            }
            _ = waker.notified() => {
                flush_outbox(helper, room, link).await?;
            }
            _ = tick.tick() => {
                sync_from_home(helper, room, link).await?;
                flush_outbox(helper, room, link).await?;
            }
            _ = shutdown.changed() => return Ok(()),
        }
    }
}

/// Pull everything after our chain head from the home.
pub async fn sync_from_home(helper: &Helper, room: &Room, link: &Arc<dyn Link>) -> Result<()> {
    loop {
        let (have, _) = helper.store.chain_head(&room.id)?;
        let req = Wire::Sync {
            room_id: room.id.clone(),
            have_seq: have,
        };
        match link.request(&req).await?.into_result()? {
            Wire::Messages {
                members,
                messages,
                head_seq,
                ..
            } => {
                for m in &members {
                    // Keep our own identity binding; upsert keeps it via COALESCE.
                    helper.store.upsert_member(m, None)?;
                }
                let n = messages.len();
                helper.apply_from_home(room, &messages)?;
                if n == 0 || (n as u32) < SYNC_BATCH || head_seq <= have + n as u64 {
                    return Ok(());
                }
            }
            other => return Err(Error::Invalid(format!("unexpected {}", other.label()))),
        }
    }
}

/// Send every queued message to the home, in order.
pub async fn flush_outbox(helper: &Helper, room: &Room, link: &Arc<dyn Link>) -> Result<()> {
    let lock = helper.submit_lock(&room.id).await;
    let _guard = lock.lock().await;
    for msg in helper.store.outbox_list(&room.id)? {
        match submit(helper, room, link, &msg).await {
            Ok(_) => {}
            Err(Error::ReachedNobody(e)) => {
                helper.store.outbox_note_failure(&msg.id, &e)?;
                return Err(Error::ReachedNobody(e));
            }
            Err(Error::RoomPaused(e)) => {
                // Nothing moves until resume. Keep it queued.
                helper.store.outbox_note_failure(&msg.id, &e)?;
                return Ok(());
            }
            Err(e) => {
                warn!(room = %room.id, id = %msg.id, error = %e, "queued message refused by home; dropped");
                helper.store.outbox_remove(&msg.id)?;
            }
        }
    }
    Ok(())
}

/// Submit one message to the home and store the sequenced copy.
pub async fn submit(
    helper: &Helper,
    room: &Room,
    link: &Arc<dyn Link>,
    msg: &Message,
) -> Result<Message> {
    let req = Wire::Submit {
        room_id: room.id.clone(),
        message: msg.clone(),
    };
    match link.request(&req).await?.into_result()? {
        Wire::Sequenced { message } => {
            match helper.apply_from_home(room, std::slice::from_ref(&message)) {
                Ok(_) => {}
                Err(Error::Invalid(_)) => {
                    // We are behind. Catch up; the sync brings this one too.
                    sync_from_home(helper, room, link).await?;
                }
                Err(e) => return Err(e),
            }
            helper.store.outbox_remove(&msg.id)?;
            Ok(message)
        }
        other => Err(Error::Invalid(format!("unexpected {}", other.label()))),
    }
}
