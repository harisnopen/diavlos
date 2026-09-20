//! The local protocol between a command (or the MCP server) and the helper.
//!
//! One JSON line in, one JSON line out, over the local socket.

use diavlos_core::{DataClass, Member, Message, MessageType, Role, Room};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A draft as the CLI or MCP sends it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DraftWire {
    pub text: String,
    #[serde(default, rename = "type")]
    pub kind: Option<MessageType>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub trace: Option<String>,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub class: Option<DataClass>,
    #[serde(default)]
    pub action: Option<diavlos_core::Action>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Hello,
    Status,
    Stop,
    Rooms,
    NewRoom {
        name: String,
        about: String,
        identity: String,
    },
    Invite {
        room: String,
        name: String,
        human: bool,
        for_node: Option<String>,
        role: Option<Role>,
        identity: String,
    },
    Join {
        invite: String,
        identity: String,
    },
    Send {
        room: String,
        identity: String,
        draft: DraftWire,
    },
    Next {
        room: String,
        identity: String,
        timeout_secs: u64,
    },
    Read {
        room: String,
        identity: String,
        since: Option<u64>,
        limit: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub result: Value,
}

impl Response {
    pub fn ok<T: Serialize>(v: T) -> Response {
        Response {
            ok: true,
            code: None,
            error: None,
            result: serde_json::to_value(v).unwrap_or(Value::Null),
        }
    }

    pub fn err(e: &diavlos_core::Error) -> Response {
        Response {
            ok: false,
            code: Some(e.code()),
            error: Some(e.to_string()),
            result: Value::Null,
        }
    }
}

// ---- result shapes -------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloResult {
    pub version: String,
    pub node: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomStatus {
    pub name: String,
    pub id: String,
    pub home: bool,
    pub connected: bool,
    pub members: usize,
    pub messages: u64,
    pub queued: u64,
    pub me: Vec<String>,
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResult {
    pub version: String,
    pub node: String,
    pub home_dir: String,
    pub rooms: Vec<RoomStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteResult {
    pub invite: String,
    pub name: String,
    pub role: Role,
    pub expires: String,
    pub for_node: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinResult {
    pub room: Room,
    pub name: String,
    pub members: Vec<Member>,
    pub messages: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendResult {
    pub message: Message,
    /// True once the room's home has given it a place in the chain. False
    /// means it is on disk here and will go when the home is reachable.
    pub delivered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadResult {
    pub messages: Vec<Message>,
    pub bookmark: u64,
}
