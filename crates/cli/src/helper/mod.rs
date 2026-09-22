//! The helper daemon. One per computer, serving many agents and keys.
//!
//! It keeps connections open, holds the inbox, signs with the keys it
//! holds, and answers commands over a local socket only your user can
//! open. Rooms this helper made are "home" here: this helper gives every
//! message its place in the chain. For rooms joined from elsewhere, this
//! helper keeps a link to the room's home and syncs what it missed.

mod local;
mod peers;

mod keys;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use diavlos_client::proto::Event;
use diavlos_client::Paths;
use diavlos_core::{
    message::now_ts, AgentInfo, ControlOp, DefaultHook, Draft, Error, Identity, Kind, Member,
    Message, MessageType, Policy, PolicyHook, Result, Room, Store, PROTOCOL_VERSION,
};
use serde_json::Value;
use tokio::sync::{broadcast, watch, Mutex, Notify};
use tracing::{info, warn};

use crate::config::Config;
use crate::net::iroh::IrohTransport;
use crate::net::{Link, Transport};

/// How many events `events` (without --follow) can replay.
const EVENT_LOG_CAP: usize = 1000;

/// Home side: room id -> member node id -> link.
type RoomLinks = HashMap<String, HashMap<String, Arc<dyn Link>>>;

/// The running helper. Shared by every task.
pub struct Helper {
    pub paths: Paths,
    pub config: Config,
    pub store: Store,
    pub net: Arc<dyn Transport>,
    /// (room id, new head seq) whenever a message lands.
    notify: broadcast::Sender<(String, u64)>,
    /// Home side: links from member helpers, per room, per node.
    links: Mutex<RoomLinks>,
    /// Member side: the link to each room's home, while it is up.
    home_links: Mutex<HashMap<String, Arc<dyn Link>>>,
    /// Member side: wake the room task to flush the outbox.
    wake: Mutex<HashMap<String, Arc<Notify>>>,
    /// Member side: one submission to the home at a time, per room.
    submit_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    identities: Mutex<HashMap<String, Arc<Identity>>>,
    /// Same-key-two-machines alerts already raised: "room/name/node".
    alerted: Mutex<HashSet<String>>,
    shutdown: watch::Sender<bool>,
    policy_hook: DefaultHook,
    /// Rooms that already have a link task running (member side).
    room_tasks: Mutex<HashSet<String>>,
    /// Everything the helper does, as a stream and a short log.
    events: broadcast::Sender<Event>,
    event_log: std::sync::Mutex<VecDeque<Event>>,
    pub metrics_addr: Option<String>,
}

impl Helper {
    /// Subscribe to "a message landed" events.
    pub fn subscribe(&self) -> broadcast::Receiver<(String, u64)> {
        self.notify.subscribe()
    }

    pub fn shutdown_signal(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    pub fn request_shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    fn notify_room(&self, room_id: &str, seq: u64) {
        let _ = self.notify.send((room_id.to_string(), seq));
    }

    /// Record something the helper did. Never content, never keys.
    pub fn emit(&self, kind: &str, room: Option<&str>, detail: Value) {
        let ev = Event {
            ts: now_ts(),
            kind: kind.to_string(),
            room: room.map(String::from),
            detail,
        };
        if let Ok(mut log) = self.event_log.lock() {
            if log.len() >= EVENT_LOG_CAP {
                log.pop_front();
            }
            log.push_back(ev.clone());
        }
        metrics::counter!("diavlos_events_total", "kind" => kind.to_string()).increment(1);
        let _ = self.events.send(ev);
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn recent_events(&self) -> Vec<Event> {
        self.event_log
            .lock()
            .map(|l| l.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Does this helper refuse to hold this class of data?
    pub fn refuses_class(&self, class: diavlos_core::DataClass) -> bool {
        self.config.helper.refuse_classes.contains(&class)
    }

    /// Apply a control message to local state. Both the home (after it
    /// sequences one) and members (when one arrives) do this.
    pub fn apply_control(&self, room: &Room, op: &ControlOp) -> Result<()> {
        match op {
            ControlOp::Grant { name, role, until } => {
                self.store
                    .set_member_role(&room.id, name, *role, until.as_deref())?;
            }
            ControlOp::Pause | ControlOp::Resume => {
                let mut r = room.clone();
                r.paused = matches!(op, ControlOp::Pause);
                self.store.update_room(&r)?;
            }
            ControlOp::Mute { name } => {
                self.store.set_member_muted(&room.id, name, true)?;
            }
            ControlOp::Unmute { name } => {
                self.store.set_member_muted(&room.id, name, false)?;
            }
            ControlOp::Revoke { name } => {
                self.store.set_member_revoked(&room.id, name)?;
            }
            ControlOp::Hold { on } => {
                let mut r = room.clone();
                r.hold = *on;
                self.store.update_room(&r)?;
            }
            ControlOp::Rotated { new_room_id } => {
                if !self.is_home(room) {
                    let mut r = room.clone();
                    r.closed = true;
                    r.paused = true;
                    r.about = format!("rotated to {new_room_id}; re-invite needed");
                    self.store.update_room(&r)?;
                    let stamp = now_ts().replace([':', '-'], "");
                    let _ = self.store.rename_room(
                        &room.id,
                        &format!("{}-rotated-{}", room.name, &stamp[..15.min(stamp.len())]),
                    );
                }
            }
        }
        self.emit(
            "control",
            Some(&room.id),
            serde_json::to_value(op).unwrap_or(Value::Null),
        );
        Ok(())
    }

    /// Is a member helper's node online in a room right now?
    pub async fn node_online(&self, room_id: &str, node: &str) -> bool {
        self.links
            .lock()
            .await
            .get(room_id)
            .and_then(|m| m.get(node))
            .map(|l| !l.is_closed())
            .unwrap_or(false)
    }

    /// One key, two machines: a member seen from a node other than the one
    /// it is bound to. If the bound node is still online, refuse and tell
    /// the owner. If it is not, the member moved; bind the new node.
    pub async fn node_check(&self, room: &Room, member: &Member, seen: &str) -> Result<()> {
        let Some(bound) = &member.node else {
            return Ok(());
        };
        if bound == seen {
            return Ok(());
        }
        if self.node_online(&room.id, bound).await {
            self.alert_key_reuse(room, member, seen).await;
            return Err(Error::Denied(format!(
                "the key for {} is already online from another machine",
                member.name
            )));
        }
        self.store
            .touch_member(&room.id, &member.name, Some(seen), &now_ts())?;
        self.store.clear_member_node(&room.id, &member.name)?;
        self.store
            .touch_member(&room.id, &member.name, Some(seen), &now_ts())?;
        self.emit(
            "member_moved",
            Some(&room.id),
            serde_json::json!({"name": member.name, "from": bound, "to": seen}),
        );
        Ok(())
    }

    /// Load a key file, or make one on first use. `default` is a human
    /// (the person who installed Diavlos); any other label is an agent.
    pub async fn identity(&self, label: &str) -> Result<Arc<Identity>> {
        diavlos_core::names::validate_name(label).map_err(|_| {
            Error::Invalid(format!(
                "identity label {label:?} must be a plain lowercase name"
            ))
        })?;
        let mut ids = self.identities.lock().await;
        if let Some(id) = ids.get(label) {
            return Ok(id.clone());
        }
        let path = self.paths.key(label);
        let id = if path.exists() {
            keys::load_identity(&self.paths, label, self.config.helper.keychain)?
        } else {
            let (name, kind) = if label == "default" {
                let user = std::env::var("USER")
                    .or_else(|_| std::env::var("USERNAME"))
                    .unwrap_or_default();
                (
                    diavlos_core::names::sanitize_name(&user).unwrap_or_else(|| "owner".into()),
                    Kind::Human,
                )
            } else {
                (label.to_string(), Kind::Agent)
            };
            let id = Identity::generate(&name, kind);
            keys::save_identity(&self.paths, label, &id, self.config.helper.keychain)?;
            info!(identity = label, kind = %kind, fingerprint = %id.public().fingerprint(), "made a new key");
            self.emit(
                "key_made",
                None,
                serde_json::json!({"identity": label, "kind": kind}),
            );
            id
        };
        let id = Arc::new(id);
        ids.insert(label.to_string(), id.clone());
        Ok(id)
    }

    /// The local identity that holds a room's owner key, if any.
    pub async fn owner_identity(&self, room: &Room) -> Result<Option<(Arc<Identity>, Member)>> {
        for lm in self.store.local_members(&room.id)? {
            if lm.member.key == room.owner {
                let id = self.identity(&lm.identity).await?;
                return Ok(Some((id, lm.member)));
            }
        }
        Ok(None)
    }

    pub fn is_home(&self, room: &Room) -> bool {
        room.home_node == self.net.node_id()
    }

    fn policy(&self, room: &Room) -> Policy {
        Policy::load(&self.paths.policy(&room.name)).unwrap_or_default()
    }

    // ---- storing messages, with all the checks ------------------------

    /// Check a message that arrived from elsewhere: known sender, good
    /// signature, owner-only types signed by the owner. Then apply what a
    /// system message says (a member joined) so this helper can verify the
    /// new member's messages later.
    fn verify_incoming(&self, room: &Room, msg: &Message) -> Result<Member> {
        if msg.room != room.id {
            return Err(Error::Invalid("message is for another room".into()));
        }
        let member = self
            .store
            .member_by_name(&room.id, &msg.from)?
            .ok_or_else(|| Error::Denied(format!("unknown sender {}", msg.from)))?;
        msg.verify(&member.key)?;
        if matches!(msg.kind, MessageType::System | MessageType::Control)
            && member.key != room.owner
        {
            return Err(Error::Denied(format!(
                "{} is not the owner and may not send {}",
                msg.from, msg.kind
            )));
        }
        if msg.kind == MessageType::System {
            if let Some(joined) = msg.data.get("member") {
                if let Ok(mut m) = serde_json::from_value::<Member>(joined.clone()) {
                    m.room_id = room.id.clone();
                    m.granted_by = room.owner;
                    self.store.upsert_member(&m, None)?;
                }
            }
        }
        if msg.kind == MessageType::Control {
            if let Ok(op) = ControlOp::from_message(msg) {
                self.apply_control(room, &op)?;
            }
        }
        Ok(member)
    }

    /// Store sequenced messages that came from the room's home. Returns
    /// `Err(Invalid)` on a chain break; the caller then syncs.
    pub fn apply_from_home(&self, room: &Room, messages: &[Message]) -> Result<u64> {
        let mut last = 0;
        for msg in messages {
            self.verify_incoming(room, msg)?;
            let stored = if self.refuses_class(msg.class) && !msg.tombstone {
                // Refuse to store the content; keep the envelope so the
                // chain stays whole.
                let t = msg.tombstone();
                let written = self.store.append(&t)?;
                if written {
                    self.emit(
                        "refused_class",
                        Some(&room.id),
                        serde_json::json!({"seq": msg.seq, "class": msg.class}),
                    );
                }
                written
            } else {
                self.store.append(msg)?
            };
            if stored {
                metrics::counter!("diavlos_messages_total", "how" => "received").increment(1);
                self.emit(
                    "message",
                    Some(&room.id),
                    serde_json::json!({"seq": msg.seq, "id": msg.id, "from": msg.from, "type": msg.kind}),
                );
                self.notify_room(&room.id, msg.seq);
            }
            last = msg.seq;
        }
        Ok(last)
    }

    /// Home side: give a signed message its place, after every check. This
    /// is the one door every message goes through on the home.
    pub async fn sequence_here(
        &self,
        room: &Room,
        mut msg: Message,
        from_node: Option<&str>,
    ) -> Result<Message> {
        if !self.is_home(room) {
            return Err(Error::Denied(
                "this helper is not the home of that room".into(),
            ));
        }
        let now = now_ts();
        let member = self
            .store
            .member_by_name(&room.id, &msg.from)?
            .ok_or_else(|| Error::Denied(format!("unknown sender {}", msg.from)))?;
        msg.verify(&member.key)?;
        msg.check_ts(chrono::Utc::now())?;
        if let Some(seen) = from_node {
            self.node_check(room, &member, seen).await?;
        }
        if room.closed {
            return Err(Error::Denied(format!(
                "room {} was rotated; ask the owner for a new invite",
                room.name
            )));
        }
        let is_owner = member.key == room.owner;
        if room.paused && !(is_owner && msg.kind == MessageType::Control) {
            return Err(Error::RoomPaused(room.name.clone()));
        }
        member.check_may_send(msg.kind, is_owner, &now)?;
        msg.check_size()?;
        if msg.room != room.id {
            return Err(Error::Invalid("message is for another room".into()));
        }
        if self.refuses_class(msg.class) {
            return Err(Error::Denied(format!(
                "this room's home refuses {} messages",
                msg.class
            )));
        }
        self.check_typed(room, &msg)?;
        let control_op = if msg.kind == MessageType::Control {
            Some(ControlOp::from_message(&msg)?)
        } else {
            None
        };
        let policy = self.policy(room);
        self.policy_hook.check(&policy, &msg)?;
        let alert = self
            .store
            .check_limits(&room.id, &msg.from, &self.config.limits)?;
        self.store.sequence_and_append(&mut msg)?;
        self.store
            .touch_member(&room.id, &member.name, from_node, &now)?;
        info!(room = %room.id, seq = msg.seq, from = %msg.from, kind = %msg.kind, "sequenced");
        metrics::counter!("diavlos_messages_total", "how" => "sequenced").increment(1);
        self.emit(
            "message",
            Some(&room.id),
            serde_json::json!({"seq": msg.seq, "id": msg.id, "from": msg.from, "type": msg.kind}),
        );
        if let Some(op) = &control_op {
            self.apply_control(room, op)?;
        }
        self.notify_room(&room.id, msg.seq);
        if alert {
            self.system_message(
                room,
                &format!(
                    "burst alert: this room has used {}% of its daily budget of {} messages",
                    self.config.limits.burst_alert_percent, self.config.limits.daily_per_room
                ),
                serde_json::json!({"event": "burst_alert"}),
            )
            .await;
        }
        self.push_to_members(room, vec![msg.clone()], from_node)
            .await;
        Ok(msg)
    }

    /// The rules that come with a type: claims are first-come, releases
    /// need the holder, approves must match the exact action, replies must
    /// answer something real.
    pub(crate) fn check_typed(&self, room: &Room, msg: &Message) -> Result<()> {
        let target = match &msg.reply_to {
            Some(id) => match self.store.message_by_id(id)? {
                // Ids are global, so check the room: a claim or approve
                // here must not reach a task or question in another room.
                Some(t) if t.room == room.id => Some(t),
                _ => return Err(Error::Invalid(format!("no message {id} in this room"))),
            },
            None => None,
        };
        match msg.kind {
            MessageType::Claim => {
                let t = target.ok_or_else(|| {
                    Error::Invalid("a claim needs reply_to set to the task id".into())
                })?;
                if t.kind != MessageType::Task {
                    return Err(Error::Invalid(format!("{} is not a task", t.id)));
                }
                match self.store.claim_holder(&room.id, &t.id)? {
                    Some(h) if h == msg.from => {
                        Err(Error::Denied("you already hold this task".into()))
                    }
                    Some(h) => Err(Error::Denied(format!("already claimed by {h}"))),
                    None => Ok(()),
                }
            }
            MessageType::Release => {
                let t = target.ok_or_else(|| {
                    Error::Invalid("a release needs reply_to set to the task id".into())
                })?;
                match self.store.claim_holder(&room.id, &t.id)? {
                    Some(h) if h == msg.from => Ok(()),
                    _ => Err(Error::Denied("you do not hold this task".into())),
                }
            }
            MessageType::Approve => {
                let t = target.ok_or_else(|| {
                    Error::Invalid("an approve needs reply_to set to the question".into())
                })?;
                let action = t.action.as_ref().ok_or_else(|| {
                    Error::Denied("that question carries no action to approve".into())
                })?;
                if msg.action_hash.as_deref() != Some(action.hash().as_str()) {
                    return Err(Error::Denied(
                        "approve does not sign the exact action".into(),
                    ));
                }
                if msg.once != Some(true) || msg.expires.is_none() {
                    return Err(Error::Denied("an approve must expire and work once".into()));
                }
                Ok(())
            }
            MessageType::Deny | MessageType::Reply => Ok(()),
            _ => Ok(()),
        }
    }

    /// Home side: a helper notice signed by the owner key. Best effort.
    async fn system_message(&self, room: &Room, text: &str, data: serde_json::Value) {
        let Ok(Some((owner, owner_member))) = self.owner_identity(room).await else {
            return;
        };
        let draft = Draft {
            room: room.id.clone(),
            from: owner_member.name.clone(),
            kind: Some(MessageType::System),
            text: text.to_string(),
            data,
            ..Default::default()
        };
        let Ok(mut msg) = Message::new(draft, owner.as_ref()) else {
            return;
        };
        if let Err(e) = self.store.sequence_and_append(&mut msg) {
            warn!(room = %room.id, error = %e, "could not store system message");
            return;
        }
        self.emit(
            "system",
            Some(&room.id),
            serde_json::json!({"seq": msg.seq, "text": text}),
        );
        self.notify_room(&room.id, msg.seq);
        self.push_to_members(room, vec![msg], None).await;
    }

    async fn alert_key_reuse(&self, room: &Room, member: &Member, node: &str) {
        let key = format!("{}/{}/{}", room.id, member.name, node);
        if !self.alerted.lock().await.insert(key) {
            return;
        }
        warn!(room = %room.id, member = %member.name, node = %node, "same key seen from a second machine; refused");
        self.emit(
            "key_reuse",
            Some(&room.id),
            serde_json::json!({"name": member.name, "node": node}),
        );
        self.system_message(
            room,
            &format!(
                "alert: the key for {} showed up from a second machine and was refused",
                member.name
            ),
            serde_json::json!({"event": "key_reuse", "name": member.name, "node": node}),
        )
        .await;
    }

    /// Home side: push new messages to every connected member helper except
    /// the one they came from. Links that fail are dropped; the member
    /// syncs when it reconnects.
    async fn push_to_members(&self, room: &Room, messages: Vec<Message>, except: Option<&str>) {
        let targets: Vec<(String, Arc<dyn Link>)> = {
            let links = self.links.lock().await;
            match links.get(&room.id) {
                Some(m) => m
                    .iter()
                    .filter(|(node, link)| Some(node.as_str()) != except && !link.is_closed())
                    .map(|(n, l)| (n.clone(), l.clone()))
                    .collect(),
                None => Vec::new(),
            }
        };
        let room_id = room.id.clone();
        for (node, link) in targets {
            let msgs = messages.clone();
            let rid = room_id.clone();
            tokio::spawn(async move {
                let req = crate::net::Wire::Push {
                    room_id: rid,
                    messages: msgs,
                };
                if let Err(e) = link.request(&req).await {
                    warn!(node = %node, error = %e, "push failed; member will sync later");
                    metrics::counter!("diavlos_push_failures_total").increment(1);
                }
            });
        }
    }

    /// Home side: remember a member helper's link so we can push to it.
    /// One helper may hold many keys, so a second link from the same node
    /// simply replaces the first; the one-key-two-machines check lives in
    /// `node_check`.
    async fn register_link(&self, room_id: &str, link: Arc<dyn Link>) -> Result<()> {
        let node = link.remote_node();
        let mut links = self.links.lock().await;
        let per_room = links.entry(room_id.to_string()).or_default();
        if let Some(old) = per_room.get(&node) {
            if !Arc::ptr_eq(old, &link) && !old.is_closed() {
                // Keep the newer connection; the old one will notice on
                // its next request and reconnect if it is still alive.
                old.close();
            }
        }
        per_room.insert(node, link);
        Ok(())
    }

    // ---- member side ---------------------------------------------------

    /// Start the link task for a room joined from elsewhere, once.
    pub async fn ensure_room_task(self: &Arc<Self>, room_id: &str) {
        if self.room_tasks.lock().await.insert(room_id.to_string()) {
            tokio::spawn(peers::room_link_task(self.clone(), room_id.to_string()));
        }
    }

    /// Wake the room task so it flushes the outbox now.
    async fn wake_room(&self, room_id: &str) {
        let n = self.waker(room_id).await;
        n.notify_one();
    }

    /// The per-room lock that keeps `send` and the outbox flush from
    /// submitting the same message twice.
    pub async fn submit_lock(&self, room_id: &str) -> Arc<Mutex<()>> {
        let mut l = self.submit_locks.lock().await;
        l.entry(room_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Member side: the current link to a room's home, if it is up.
    pub async fn home_link(&self, room_id: &str) -> Option<Arc<dyn Link>> {
        self.home_links
            .lock()
            .await
            .get(room_id)
            .filter(|l| !l.is_closed())
            .cloned()
    }

    async fn waker(&self, room_id: &str) -> Arc<Notify> {
        let mut w = self.wake.lock().await;
        w.entry(room_id.to_string())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    }

    /// Build a message from a local identity as a member of a room.
    pub(crate) fn draft_from(
        &self,
        room: &Room,
        member: &Member,
        identity: &Identity,
        draft: diavlos_client::proto::DraftWire,
        extra: Draft,
    ) -> Result<Message> {
        let agent = if identity.kind == Kind::Human {
            None
        } else {
            Some(AgentInfo {
                vendor: identity.profile.vendor.clone(),
                model: identity.profile.model.clone(),
                owner: identity.profile.owner.clone(),
            })
        };
        Message::new(
            Draft {
                room: room.id.clone(),
                from: member.name.clone(),
                kind: draft.kind,
                text: draft.text,
                action: draft.action,
                data: draft.data,
                reply_to: draft.reply_to,
                to: draft.to,
                trace: draft.trace,
                class: Some(draft.class.unwrap_or(room.class)),
                agent,
                action_hash: extra.action_hash,
                expires: extra.expires,
                once: extra.once,
            },
            identity,
        )
    }
}

/// Run the helper until told to stop.
pub async fn run(paths: Paths) -> anyhow::Result<()> {
    paths.ensure()?;
    Config::ensure(&paths.config())?;
    let config = Config::load(&paths.config())?;
    init_logging(&paths, &config.helper.log_level)?;
    match run_inner(paths, config).await {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::error!(error = format!("{e:#}"), "helper failed");
            tokio::time::sleep(Duration::from_millis(50)).await;
            Err(e)
        }
    }
}

async fn run_inner(paths: Paths, mut config: Config) -> anyhow::Result<()> {
    // Refuse to run twice.
    let client = diavlos_client::Client::new(paths.clone());
    if client.is_running().await {
        eprintln!("a helper is already running for {}", paths.home.display());
        return Ok(());
    }
    #[cfg(unix)]
    {
        let _ = std::fs::remove_file(paths.socket_file());
    }

    let inbox_key = if config.helper.encrypt_inbox {
        Some(keys::inbox_key(&paths, config.helper.keychain).context("inbox key")?)
    } else {
        None
    };
    let store = Store::open_with_key(&paths.db(), inbox_key).context("open inbox")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for suffix in ["", "-wal", "-shm"] {
            let p = paths.home.join(format!("diavlos.db{suffix}"));
            if p.exists() {
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
    let secret = IrohTransport::load_or_create_key(&paths.node_key())?;
    let net = IrohTransport::bind(
        secret,
        config.helper.port,
        config.helper.public_relays,
        &config.helper.relay_urls,
    )
    .await
    .context("bind network")?;
    if config.helper.port == 0 {
        if let Some(p) = net.bound_port() {
            config.helper.port = p;
            let _ = Config::save_port(&paths.config(), p);
        }
    }
    let net: Arc<dyn Transport> = Arc::new(net);
    info!(node = %net.node_id(), version = diavlos_core::VERSION, protocol = PROTOCOL_VERSION, "helper up");

    let metrics_addr = if config.helper.metrics_addr.trim().is_empty() {
        None
    } else {
        let addr: std::net::SocketAddr =
            config.helper.metrics_addr.parse().context("metrics_addr")?;
        if !addr.ip().is_loopback() {
            anyhow::bail!("metrics_addr must be on localhost");
        }
        metrics_exporter_prometheus::PrometheusBuilder::new()
            .with_http_listener(addr)
            .install()
            .context("metrics listener")?;
        Some(addr.to_string())
    };

    let (shutdown, _) = watch::channel(false);
    let (notify, _) = broadcast::channel(1024);
    let (events, _) = broadcast::channel(1024);
    let helper = Arc::new(Helper {
        paths: paths.clone(),
        config,
        store,
        net,
        notify,
        links: Mutex::new(HashMap::new()),
        home_links: Mutex::new(HashMap::new()),
        wake: Mutex::new(HashMap::new()),
        submit_locks: Mutex::new(HashMap::new()),
        identities: Mutex::new(HashMap::new()),
        alerted: Mutex::new(HashSet::new()),
        shutdown,
        policy_hook: DefaultHook,
        room_tasks: Mutex::new(HashSet::new()),
        events,
        event_log: std::sync::Mutex::new(VecDeque::new()),
        metrics_addr,
    });
    helper.emit(
        "helper_up",
        None,
        serde_json::json!({"node": helper.net.node_id(), "version": diavlos_core::VERSION}),
    );
    tokio::spawn(retention_loop(helper.clone()));

    // Links from other helpers.
    tokio::spawn(peers::accept_loop(helper.clone()));
    // A task per room we joined from elsewhere.
    for room in helper.store.list_rooms()? {
        if !helper.is_home(&room) && !room.closed {
            helper.ensure_room_task(&room.id).await;
        }
    }
    // Commands over the local socket.
    let listener = local::bind(&paths)?;
    let listener_handle = Arc::new(listener);
    let local_task = tokio::spawn(local::serve(helper.clone(), listener_handle.clone()));

    let mut rx = helper.shutdown_signal();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("ctrl-c"),
        _ = rx.changed() => info!("stop requested"),
    }
    helper.emit("helper_down", None, Value::Null);
    // Drop the listener first: it removes its own socket file. Removing it
    // by path here would race a freshly started helper that already bound
    // the same name.
    local_task.abort();
    drop(listener_handle);
    helper.net.shutdown().await;
    info!("helper down");
    // Let the log flush.
    tokio::time::sleep(Duration::from_millis(50)).await;
    Ok(())
}

/// Retention per room: 30 days, 7 years, or none. The helper enforces it.
/// A legal hold freezes it.
async fn retention_loop(helper: Arc<Helper>) {
    let every = std::env::var("DIAVLOS_RETENTION_CHECK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(helper.config.helper.retention_check_secs)
        .max(1);
    let mut shutdown = helper.shutdown_signal();
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(every)) => {}
            _ = shutdown.changed() => return,
        }
        let Ok(rooms) = helper.store.list_rooms() else {
            continue;
        };
        for room in rooms {
            let Some(days) = room.retention_days else {
                continue;
            };
            if room.hold {
                continue;
            }
            let cutoff = (chrono::Utc::now() - chrono::Duration::days(days as i64))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            match helper.store.tombstone_before(&room.id, &cutoff) {
                Ok(0) => {}
                Ok(n) => {
                    info!(room = %room.id, tombstoned = n, "retention");
                    helper.emit(
                        "retention",
                        Some(&room.id),
                        serde_json::json!({"tombstoned": n, "before": cutoff}),
                    );
                }
                Err(e) => warn!(room = %room.id, error = %e, "retention failed"),
            }
        }
    }
}

fn init_logging(paths: &Paths, level: &str) -> anyhow::Result<()> {
    use tracing_subscriber::{fmt, EnvFilter};
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(paths.log(), std::fs::Permissions::from_mode(0o600));
    }
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("warn,diavlos={level},diavlos_core={level}")));
    let _ = fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(std::sync::Mutex::new(file))
        .try_init();
    Ok(())
}
