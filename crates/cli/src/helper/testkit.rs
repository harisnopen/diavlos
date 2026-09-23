//! Test-only: helpers wired together in memory, and links that fail on
//! cue. No sockets, no relays, no timing luck.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use diavlos_client::Paths;
use diavlos_core::{DefaultHook, Error, Result, Store};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex};

use super::Helper;
use crate::config::Config;
use crate::net::{Link, Reply, Transport, Wire};

/// A transport that never dials: tests open links themselves.
pub struct NoNet {
    node: String,
}

#[async_trait]
impl Transport for NoNet {
    fn node_id(&self) -> String {
        self.node.clone()
    }
    fn hints(&self) -> Value {
        Value::Null
    }
    async fn dial(&self, node: &str, _hints: &Value) -> Result<Arc<dyn Link>> {
        Err(Error::ReachedNobody(format!(
            "{node} is offline in this test"
        )))
    }
    async fn accept(&self) -> Option<Arc<dyn Link>> {
        std::future::pending().await
    }
    fn network_info(&self) -> Value {
        Value::Null
    }
    async fn shutdown(&self) {}
}

type Incoming = (Wire, oneshot::Sender<Wire>);

/// One end of an in-memory link. Every frame goes through JSON and back,
/// as it would on the wire.
pub struct MemLink {
    remote: String,
    out: mpsc::UnboundedSender<Incoming>,
    inbox: Mutex<mpsc::UnboundedReceiver<Incoming>>,
    closed: AtomicBool,
}

fn roundtrip(w: &Wire) -> Wire {
    serde_json::from_str(&serde_json::to_string(w).unwrap()).unwrap()
}

/// Two linked ends: `a` is at node `a_node` and talks to `b_node`.
pub fn mem_pair(a_node: &str, b_node: &str) -> (Arc<MemLink>, Arc<MemLink>) {
    let (a_tx, b_rx) = mpsc::unbounded_channel();
    let (b_tx, a_rx) = mpsc::unbounded_channel();
    let a = Arc::new(MemLink {
        remote: b_node.to_string(),
        out: a_tx,
        inbox: Mutex::new(a_rx),
        closed: AtomicBool::new(false),
    });
    let b = Arc::new(MemLink {
        remote: a_node.to_string(),
        out: b_tx,
        inbox: Mutex::new(b_rx),
        closed: AtomicBool::new(false),
    });
    (a, b)
}

struct MemReply(oneshot::Sender<Wire>);

#[async_trait]
impl Reply for MemReply {
    async fn send(self: Box<Self>, resp: &Wire) -> Result<()> {
        self.0
            .send(roundtrip(resp))
            .map_err(|_| Error::ReachedNobody("link closed".into()))
    }
}

#[async_trait]
impl Link for MemLink {
    fn remote_node(&self) -> String {
        self.remote.clone()
    }
    async fn request(&self, req: &Wire) -> Result<Wire> {
        if self.is_closed() {
            return Err(Error::ReachedNobody("link closed".into()));
        }
        let (tx, rx) = oneshot::channel();
        self.out
            .send((roundtrip(req), tx))
            .map_err(|_| Error::ReachedNobody("link closed".into()))?;
        rx.await
            .map_err(|_| Error::ReachedNobody("link closed".into()))
    }
    async fn next_request(&self) -> Result<(Wire, Box<dyn Reply>)> {
        let mut inbox = self.inbox.lock().await;
        match inbox.recv().await {
            Some((w, tx)) => Ok((w, Box::new(MemReply(tx)))),
            None => Err(Error::ReachedNobody("link closed".into())),
        }
    }
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

/// What a scripted link does with the next matching request.
#[derive(Debug)]
pub enum Fault {
    /// The request never reaches the other side (a write that broke
    /// mid-frame).
    FailBefore(Error),
    /// The other side gets it and answers, and the answer is lost (a read
    /// that broke mid-frame).
    FailAfter(Error),
    /// The other side is not asked; this is the answer.
    Answer(Box<Wire>),
}

/// A link that plays faults, in order, on requests with a given label.
/// Everything else passes through.
pub struct ScriptedLink {
    inner: Arc<dyn Link>,
    label: &'static str,
    script: std::sync::Mutex<VecDeque<Fault>>,
}

impl ScriptedLink {
    pub fn new(inner: Arc<dyn Link>, label: &'static str) -> Arc<ScriptedLink> {
        Arc::new(ScriptedLink {
            inner,
            label,
            script: std::sync::Mutex::new(VecDeque::new()),
        })
    }
    pub fn then(&self, f: Fault) -> &Self {
        self.script.lock().unwrap().push_back(f);
        self
    }
}

#[async_trait]
impl Link for ScriptedLink {
    fn remote_node(&self) -> String {
        self.inner.remote_node()
    }
    async fn request(&self, req: &Wire) -> Result<Wire> {
        let next = if req.label() == self.label {
            self.script.lock().unwrap().pop_front()
        } else {
            None
        };
        match next {
            None => self.inner.request(req).await,
            Some(Fault::FailBefore(e)) => Err(e),
            Some(Fault::FailAfter(e)) => {
                let _ = self.inner.request(req).await;
                Err(e)
            }
            Some(Fault::Answer(w)) => Ok(roundtrip(&w)),
        }
    }
    async fn next_request(&self) -> Result<(Wire, Box<dyn Reply>)> {
        self.inner.next_request().await
    }
    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
    fn close(&self) {
        self.inner.close()
    }
}

/// A temp directory removed when the test ends.
pub struct TempHome(pub PathBuf);

impl TempHome {
    /// A fresh directory. Tests run in parallel and some clocks (macOS) are
    /// coarser than a nanosecond, so a counter keeps two names apart.
    pub fn new(tag: &str) -> TempHome {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "diavlos-kit-{tag}-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TempHome(dir)
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A helper on node `node`, its store in `home`. Opening the same home
/// again is a restart.
pub fn helper(node: &str, home: &TempHome, config: Option<Config>) -> Arc<Helper> {
    let paths = Paths {
        home: home.0.clone(),
    };
    paths.ensure().unwrap();
    let mut config = config.unwrap_or_default();
    config.helper.keychain = false;
    let store = Store::open_with_key(&paths.db(), Some([9u8; 32])).unwrap();
    let (shutdown, _) = watch::channel(false);
    let (notify, _) = broadcast::channel(1024);
    let (events, _) = broadcast::channel(1024);
    Arc::new(Helper {
        paths,
        config,
        store,
        net: Arc::new(NoNet {
            node: node.to_string(),
        }),
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
        metrics_addr: None,
        faults: Default::default(),
        clock_skew: Default::default(),
    })
}

/// A link from `member` to `home`, with `home` serving it and the hello
/// said. The member's room task is not involved: tests drive the link.
pub async fn connect(home: &Arc<Helper>, member: &Arc<Helper>) -> Arc<dyn Link> {
    let (m_end, h_end) = mem_pair(&member.net.node_id(), &home.net.node_id());
    tokio::spawn(super::peers::serve_link(home.clone(), h_end));
    let link: Arc<dyn Link> = m_end;
    let hello = Wire::Hello {
        v: diavlos_core::PROTOCOL_VERSION,
        node: member.net.node_id(),
        version: diavlos_core::VERSION.into(),
    };
    link.request(&hello).await.unwrap().into_result().unwrap();
    link
}

/// `home` makes room `room` as `default`; each of `members` (helper, key
/// label, name in the room, human?) joins it over a link of its own.
pub async fn room_with(
    home: &Arc<Helper>,
    room: &str,
    members: &[(&Arc<Helper>, &str, &str, bool)],
) -> diavlos_core::Room {
    let r = super::local::new_room(home, room, "", "default", None, None)
        .await
        .unwrap();
    for (m, label, name, human) in members {
        let inv = super::local::invite(home, room, name, *human, None, None, "default")
            .await
            .unwrap();
        if Arc::ptr_eq(m, home) {
            super::local::join(m, &inv.invite, label).await.unwrap();
        } else {
            let link = connect(home, m).await;
            super::local::join_over(m, &link, &inv.invite, label)
                .await
                .unwrap();
        }
    }
    r
}

/// Sign a chat message as `label` in `room`, without sending it.
pub async fn draft(
    helper: &Arc<Helper>,
    room: &str,
    label: &str,
    text: &str,
) -> diavlos_core::Message {
    draft_as(
        helper,
        room,
        label,
        text,
        diavlos_core::MessageType::Chat,
        None,
    )
    .await
}

pub async fn draft_as(
    helper: &Arc<Helper>,
    room: &str,
    label: &str,
    text: &str,
    kind: diavlos_core::MessageType,
    reply_to: Option<&str>,
) -> diavlos_core::Message {
    let room = helper.store.room(room).unwrap();
    let lm = helper.store.local_member(&room.id, label).unwrap().unwrap();
    let id = helper.identity(label).await.unwrap();
    helper
        .draft_from(
            &room,
            &lm.member,
            &id,
            diavlos_client::proto::DraftWire {
                text: text.into(),
                kind: Some(kind),
                reply_to: reply_to.map(String::from),
                ..Default::default()
            },
            Default::default(),
        )
        .unwrap()
}

/// Make `link` the member's link to the home of `room_id`, as the room task
/// would once connected.
pub async fn set_home_link(member: &Arc<Helper>, room_id: &str, link: Arc<dyn Link>) {
    member
        .home_links
        .lock()
        .await
        .insert(room_id.to_string(), link);
}
