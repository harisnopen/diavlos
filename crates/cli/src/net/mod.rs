//! The network layer, behind one interface.
//!
//! Everything above this module talks in [`Wire`] messages over a [`Link`].
//! The only implementation today is iroh (`net::iroh`). Swap it by
//! implementing [`Transport`] and [`Link`] again; nothing else changes.

pub mod iroh;

use std::sync::Arc;

use async_trait::async_trait;
use diavlos_core::{Error, Member, Message, Result, Room, SignedProfile};
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
    /// "Give me what I am missing after this seq."
    Sync {
        room_id: String,
        have_seq: u64,
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
    Err {
        code: i32,
        msg: String,
    },
}

impl Wire {
    pub fn error(e: &Error) -> Wire {
        Wire::Err {
            code: e.code(),
            msg: e.to_string(),
        }
    }

    /// Turn an `Err` frame into a Rust error.
    pub fn into_result(self) -> Result<Wire> {
        match self {
            Wire::Err { code, msg } => Err(Error::from_code(code, &msg)),
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
    async fn shutdown(&self);
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
            have_seq: 7,
        };
        write_frame(&mut a, &w).await.unwrap();
        let back = read_frame(&mut b).await.unwrap();
        assert!(matches!(back, Wire::Sync { have_seq: 7, .. }));
    }
}
