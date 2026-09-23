//! The local protocol between a command (or the MCP server) and the helper.
//!
//! One JSON line in, one JSON line out, over the local socket.

use diavlos_core::{DataClass, Kind, Member, Message, MessageType, Role, Room};
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
        #[serde(default)]
        retention_days: Option<u32>,
        #[serde(default)]
        class: Option<DataClass>,
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
    /// Send a question and wait for a reply to that exact message.
    Ask {
        room: String,
        identity: String,
        draft: DraftWire,
        timeout_secs: u64,
    },
    Who {
        room: String,
    },
    Claim {
        room: String,
        identity: String,
        task_id: String,
    },
    Release {
        room: String,
        identity: String,
        task_id: String,
    },
    /// Owner: a control message (grant, pause, resume, mute, unmute,
    /// revoke, hold).
    Control {
        room: String,
        identity: String,
        control: diavlos_core::ControlOp,
    },
    /// Human: say no to an ask.
    Deny {
        room: String,
        identity: String,
        msg_id: String,
        reason: String,
    },
    /// Human: say yes to an ask. Signs the exact action.
    Approve {
        room: String,
        identity: String,
        msg_id: String,
    },
    /// Exit 0 if a valid, unexpired, unused human approve exists for
    /// exactly this action. Spends it.
    CheckApprove {
        room: String,
        action: diavlos_core::Action,
    },
    Export {
        room: String,
        identity: String,
        since: Option<String>,
    },
    /// Owner: new room key. Everyone out.
    Rotate {
        room: String,
        identity: String,
    },
    /// Streaming: JSON lines until the client goes away.
    Events {
        follow: bool,
    },
    /// Streaming: messages from the bookmark onward, then live.
    Watch {
        room: String,
        identity: String,
    },
    /// Who a local identity label is: its name, kind and key. Makes the key
    /// on first use, as every other request that names an identity does.
    Whoami {
        identity: String,
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
pub struct WhoamiResult {
    /// The local label, as passed to `--as`.
    pub label: String,
    /// The name the key carries.
    pub name: String,
    pub kind: Kind,
    pub key: String,
    pub fingerprint: String,
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
    /// What the network layer says about relays and addresses.
    #[serde(default)]
    pub network: Value,
    #[serde(default)]
    pub encrypted_inbox: bool,
    #[serde(default)]
    pub metrics_addr: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhoEntry {
    pub name: String,
    pub kind: String,
    pub role: String,
    pub fingerprint: String,
    pub key: String,
    /// What they said they do: vendor, model, tools, owner.
    #[serde(default)]
    pub profile: Value,
    #[serde(default)]
    pub last_seen: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    pub owner: bool,
    pub muted: bool,
    pub revoked: bool,
    pub expired: bool,
    #[serde(default)]
    pub node: Option<String>,
    pub online: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskResult {
    pub question: Message,
    pub reply: Message,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckApproveResult {
    pub approve_id: String,
    pub approved_by: String,
    pub action_hash: String,
    pub expires: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportResult {
    pub bundle: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotateResult {
    pub old_room_id: String,
    pub old_room_name: String,
    pub room: Room,
}

/// One line of `events --follow`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub ts: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub detail: Value,
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
