//! The Diavlos library. For agents you build yourself.
//!
//! Same eight calls as the MCP tools: send, ask, next, read, claim,
//! release, who, rooms. Everything goes through the local helper, which
//! is started on first use.
//!
//! ```no_run
//! # async fn demo() -> diavlos_core::Result<()> {
//! use diavlos_client::{Paths, Room, SendOptions};
//! let paths = Paths::resolve(None).unwrap();
//! let room = Room::join(&paths, "dv1.…", "my-bot").await?;
//! loop {
//!     let msg = room.next(0).await?;
//!     if msg.kind == diavlos_core::MessageType::Task {
//!         room.send("done", SendOptions::done(&msg.id)).await?;
//!     }
//! }
//! # }
//! ```

pub mod client;
pub mod paths;
pub mod proto;

pub use client::Client;
pub use paths::Paths;

use diavlos_core::{Message, MessageType, Result};
use proto::{DraftWire, JoinResult, ReadResult, Request, RoomStatus, SendResult, WhoEntry};

/// Options for `send`.
#[derive(Debug, Clone, Default)]
pub struct SendOptions {
    pub kind: Option<MessageType>,
    pub to: Option<String>,
    pub reply_to: Option<String>,
    pub trace: Option<String>,
    pub data: serde_json::Value,
    pub action: Option<diavlos_core::Action>,
}

impl SendOptions {
    pub fn kind(kind: MessageType) -> Self {
        SendOptions {
            kind: Some(kind),
            ..Default::default()
        }
    }
    /// A `done` answering a task.
    pub fn done(task_id: &str) -> Self {
        SendOptions {
            kind: Some(MessageType::Done),
            reply_to: Some(task_id.into()),
            ..Default::default()
        }
    }
    /// A `reply` answering a message.
    pub fn reply(msg_id: &str) -> Self {
        SendOptions {
            kind: Some(MessageType::Reply),
            reply_to: Some(msg_id.into()),
            ..Default::default()
        }
    }
}

/// One room, seen as one identity.
#[derive(Debug, Clone)]
pub struct Room {
    client: Client,
    pub room: String,
    pub identity: String,
    /// Your name inside the room, once known.
    pub name: Option<String>,
}

impl Room {
    /// Join with an invite. `identity` is the local key label to join as
    /// (made on first use). The name inside the room comes from the invite.
    pub async fn join(paths: &Paths, invite: &str, identity: &str) -> Result<Room> {
        let client = Client::new(paths.clone());
        let v = client
            .call(&Request::Join {
                invite: invite.to_string(),
                identity: identity.to_string(),
            })
            .await?;
        let r: JoinResult = serde_json::from_value(v)?;
        Ok(Room {
            client,
            room: r.room.name,
            identity: identity.to_string(),
            name: Some(r.name),
        })
    }

    /// A room you are already in.
    pub fn open(paths: &Paths, room: &str, identity: &str) -> Room {
        Room {
            client: Client::new(paths.clone()),
            room: room.to_string(),
            identity: identity.to_string(),
            name: None,
        }
    }

    pub async fn send(&self, text: &str, opts: SendOptions) -> Result<Message> {
        let v = self
            .client
            .call(&Request::Send {
                room: self.room.clone(),
                identity: self.identity.clone(),
                draft: DraftWire {
                    text: text.to_string(),
                    kind: opts.kind,
                    to: opts.to,
                    reply_to: opts.reply_to,
                    trace: opts.trace,
                    data: opts.data,
                    class: None,
                    action: opts.action,
                },
            })
            .await?;
        let r: SendResult = serde_json::from_value(v)?;
        Ok(r.message)
    }

    /// Send a question and wait for a reply to that exact message.
    pub async fn ask(
        &self,
        text: &str,
        timeout_secs: u64,
        action: Option<diavlos_core::Action>,
    ) -> Result<Message> {
        let v = self
            .client
            .call(&Request::Ask {
                room: self.room.clone(),
                identity: self.identity.clone(),
                draft: DraftWire {
                    text: text.to_string(),
                    kind: Some(MessageType::Question),
                    action,
                    ..Default::default()
                },
                timeout_secs,
            })
            .await?;
        let r: proto::AskResult = serde_json::from_value(v)?;
        Ok(r.reply)
    }

    /// Wait for the next message from someone else. `0` waits forever.
    pub async fn next(&self, timeout_secs: u64) -> Result<Message> {
        let v = self
            .client
            .call(&Request::Next {
                room: self.room.clone(),
                identity: self.identity.clone(),
                timeout_secs,
            })
            .await?;
        Ok(serde_json::from_value(v)?)
    }

    /// Read from your bookmark onward. Never deletes. With `since`, read
    /// from that seq instead and leave the bookmark alone.
    pub async fn read(&self, since: Option<u64>, limit: u32) -> Result<Vec<Message>> {
        let v = self
            .client
            .call(&Request::Read {
                room: self.room.clone(),
                identity: self.identity.clone(),
                since,
                limit,
            })
            .await?;
        let r: ReadResult = serde_json::from_value(v)?;
        Ok(r.messages)
    }

    /// Take a task. First wins; second is told no.
    pub async fn claim(&self, task_id: &str) -> Result<Message> {
        let v = self
            .client
            .call(&Request::Claim {
                room: self.room.clone(),
                identity: self.identity.clone(),
                task_id: task_id.to_string(),
            })
            .await?;
        let r: SendResult = serde_json::from_value(v)?;
        Ok(r.message)
    }

    /// Give a task back.
    pub async fn release(&self, task_id: &str) -> Result<Message> {
        let v = self
            .client
            .call(&Request::Release {
                room: self.room.clone(),
                identity: self.identity.clone(),
                task_id: task_id.to_string(),
            })
            .await?;
        let r: SendResult = serde_json::from_value(v)?;
        Ok(r.message)
    }

    /// Who is here.
    pub async fn who(&self) -> Result<Vec<WhoEntry>> {
        let v = self
            .client
            .call(&Request::Who {
                room: self.room.clone(),
            })
            .await?;
        Ok(serde_json::from_value(v)?)
    }
}

/// The rooms this helper knows.
pub async fn rooms(paths: &Paths) -> Result<Vec<RoomStatus>> {
    let v = Client::new(paths.clone()).call(&Request::Rooms).await?;
    Ok(serde_json::from_value(v)?)
}
