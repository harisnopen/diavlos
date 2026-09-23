//! The network layer, behind one interface.
//!
//! Everything above this module talks in [`Wire`] messages over a [`Link`].
//! The only implementation today is iroh (`net::iroh`). Swap it by
//! implementing [`Transport`] and [`Link`] again; nothing else changes.

pub mod iroh;
pub mod private;

use std::sync::Arc;

use async_trait::async_trait;
use diavlos_core::{Error, Fate, Member, Message, Result, Room, SignedProfile};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Application protocol id. Bump with the wire protocol version.
pub const ALPN: &[u8] = b"diavlos/1";
/// Largest frame we read. A message is capped at 64 KiB; a sync batch of
/// 500 messages fits well inside this.
pub const MAX_FRAME: usize = 48 * 1024 * 1024;
/// Messages per sync batch.
pub const SYNC_BATCH: u32 = 500;

/// Everything two helpers say to each other.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Wire {
    /// First thing on every link: the version handshake.
    Hello {
        v: u32,
        node: String,
        version: String,
    },
    HelloOk {
        v: u32,
        version: String,
    },
    /// Join a room with a signed invite. The joiner presents its signed
    /// profile and how it can be reached.
    Join {
        invite: String,
        profile: SignedProfile,
        hints: Value,
    },
    JoinOk {
        room: Room,
        members: Vec<Member>,
        messages: Vec<Message>,
    },
    /// A member asks the room's home to give a signed message its place in
    /// the chain.
    Submit {
        room_id: String,
        message: Message,
    },
    Sequenced {
        message: Message,
    },
    /// "Give me what I am missing after this seq." Signed by the member's
    /// key so the home knows who asks and can bind the node.
    Sync {
        room_id: String,
        name: String,
        have_seq: u64,
        ts: String,
        sig: String,
    },
    Messages {
        room_id: String,
        members: Vec<Member>,
        messages: Vec<Message>,
        head_seq: u64,
    },
    /// The home pushes new messages to a member.
    Push {
        room_id: String,
        messages: Vec<Message>,
    },
    Ack {
        seq: u64,
    },
    /// "Your push did not fit my chain; I will sync."
    NeedSync {
        room_id: String,
    },
    /// A member is about to act on an approve and asks the room's home to
    /// record the spend. Signed by the member's key over every field (see
    /// [`spend_signing_bytes`]), so the binding to this room, approve,
    /// action, operation and node cannot be swapped.
    Spend {
        room_id: String,
        approve_id: String,
        action_hash: String,
        /// Unique per operation; the same on every retry of it.
        op_id: String,
        spender: String,
        node: String,
        ts: String,
        sig: String,
    },
    /// The spend is recorded (now, or before for this same operation).
    Spent {
        record: diavlos_core::SpendRecord,
    },
    /// That approve was spent before, by another operation.
    AlreadySpent {
        approve_id: String,
    },
    /// A refusal or failure. `fate` says whether the sender should keep
    /// the message and try again (temporary) or stop (definitive); an old
    /// helper leaves it out, and the sender then goes by `code`.
    /// `retry_after` is how many seconds until trying again can work.
    Err {
        code: i32,
        msg: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fate: Option<Fate>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after: Option<u64>,
    },
}

impl Wire {
    pub fn error(e: &Error) -> Wire {
        Wire::Err {
            code: e.code(),
            msg: e.to_string(),
            fate: e.fate(),
            retry_after: e.retry_after(),
        }
    }

    /// An error frame with no advice on retrying.
    pub fn err(code: i32, msg: impl Into<String>) -> Wire {
        Wire::Err {
            code,
            msg: msg.into(),
            fate: None,
            retry_after: None,
        }
    }

    /// Turn an `Err` frame into a Rust error.
    pub fn into_result(self) -> Result<Wire> {
        match self {
            Wire::Err { code, msg, .. } => Err(Error::from_code(code, &msg)),
            w => Ok(w),
        }
    }

    /// A short label for logs. Never the content.
    pub fn label(&self) -> &'static str {
        match self {
            Wire::Hello { .. } => "hello",
            Wire::HelloOk { .. } => "hello_ok",
            Wire::Join { .. } => "join",
            Wire::JoinOk { .. } => "join_ok",
            Wire::Submit { .. } => "submit",
            Wire::Sequenced { .. } => "sequenced",
            Wire::Sync { .. } => "sync",
            Wire::Messages { .. } => "messages",
            Wire::Push { .. } => "push",
            Wire::Ack { .. } => "ack",
            Wire::NeedSync { .. } => "need_sync",
            Wire::Spend { .. } => "spend",
            Wire::Spent { .. } => "spent",
            Wire::AlreadySpent { .. } => "already_spent",
            Wire::Err { .. } => "err",
        }
    }
}

/// The answer side of one request on a link.
#[async_trait]
pub trait Reply: Send {
    async fn send(self: Box<Self>, resp: &Wire) -> Result<()>;
}

/// One open connection to another helper. Both sides can send requests.
#[async_trait]
pub trait Link: Send + Sync {
    /// The other helper's node id.
    fn remote_node(&self) -> String;
    /// Send a request and wait for its answer.
    async fn request(&self, req: &Wire) -> Result<Wire>;
    /// Wait for the other side's next request.
    async fn next_request(&self) -> Result<(Wire, Box<dyn Reply>)>;
    fn is_closed(&self) -> bool;
    fn close(&self);
}

/// The transport: how this helper is reached and how it reaches others.
#[async_trait]
pub trait Transport: Send + Sync {
    /// This helper's node id. Stable across restarts.
    fn node_id(&self) -> String;
    /// How others can reach this node right now. Opaque to everyone but
    /// the transport; carried in invites and join replies.
    fn hints(&self) -> Value;
    /// Open a link to another helper.
    async fn dial(&self, node: &str, hints: &Value) -> Result<Arc<dyn Link>>;
    /// The next link another helper opened to us. `None` once shut down.
    async fn accept(&self) -> Option<Arc<dyn Link>>;
    /// What `doctor` and `status` show: relays, addresses, sockets.
    fn network_info(&self) -> Value;
    async fn shutdown(&self);
}

/// The bytes a member signs on a `Sync`.
pub fn sync_signing_bytes(
    room_id: &str,
    name: &str,
    have_seq: u64,
    ts: &str,
    node: &str,
) -> Vec<u8> {
    diavlos_core::canonical::canonical_json(&serde_json::json!({
        "sync": 1, "room_id": room_id, "name": name, "have_seq": have_seq, "ts": ts, "node": node,
    }))
    .into_bytes()
}

/// The bytes a member signs on a `Spend`.
#[allow(clippy::too_many_arguments)]
pub fn spend_signing_bytes(
    room_id: &str,
    approve_id: &str,
    action_hash: &str,
    op_id: &str,
    spender: &str,
    node: &str,
    ts: &str,
) -> Vec<u8> {
    diavlos_core::canonical::canonical_json(&serde_json::json!({
        "spend": 1, "room_id": room_id, "approve_id": approve_id, "action_hash": action_hash,
        "op_id": op_id, "spender": spender, "node": node, "ts": ts,
    }))
    .into_bytes()
}

/// Write one length-prefixed JSON frame.
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, wire: &Wire) -> Result<()> {
    let body = serde_json::to_vec(wire)?;
    if body.len() > MAX_FRAME {
        return Err(Error::Invalid("frame too large".into()));
    }
    w.write_all(&(body.len() as u32).to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

/// Read one length-prefixed JSON frame.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Wire> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(Error::Invalid("frame too large".into()));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let w = Wire::Sync {
            room_id: "r".into(),
            name: "bob".into(),
            have_seq: 7,
            ts: "t".into(),
            sig: "s".into(),
        };
        write_frame(&mut a, &w).await.unwrap();
        let back = read_frame(&mut b).await.unwrap();
        assert!(matches!(back, Wire::Sync { have_seq: 7, .. }));
    }

    /// A frame cut off part way is an I/O error on both sides, which the
    /// outbox treats as temporary: the message is kept and sent again.
    #[tokio::test]
    async fn a_frame_cut_short_is_an_io_error() {
        // Reading: the length says 100 bytes, 10 arrive, then the stream ends.
        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_all(&100u32.to_be_bytes()).await.unwrap();
        a.write_all(b"{\"t\":\"hel").await.unwrap();
        drop(a);
        assert!(matches!(read_frame(&mut b).await, Err(Error::Io(_))));
        // Writing: the other end went away.
        let (mut a, b) = tokio::io::duplex(8);
        drop(b);
        let w = Wire::err(1, "x".repeat(64));
        assert!(matches!(write_frame(&mut a, &w).await, Err(Error::Io(_))));
        assert_eq!(
            Error::Io(std::io::Error::other("x")).fate(),
            Some(Fate::Temporary)
        );
    }

    /// An error frame without `fate` (an older helper) still reads, and a
    /// new one keeps it.
    #[test]
    fn error_frames_read_with_and_without_fate() {
        let old: Wire = serde_json::from_str(r#"{"t":"err","code":6,"msg":"denied: no"}"#).unwrap();
        assert!(matches!(old, Wire::Err { fate: None, .. }));
        let e = Error::OverBudget("daily".into(), Some(90));
        let w: Wire =
            serde_json::from_str(&serde_json::to_string(&Wire::error(&e)).unwrap()).unwrap();
        assert!(matches!(
            w,
            Wire::Err {
                fate: Some(Fate::Temporary),
                retry_after: Some(90),
                ..
            }
        ));
    }
}
