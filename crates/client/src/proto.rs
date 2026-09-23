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
    /// Hand out the next message owed to this identity. With `manual_ack`
    /// the answer is a [`NextResult`]: the message and a lease token, and
    /// the message stays this reader's until it acks, nacks or the lease
    /// runs out. Without it (older callers) the answer is the message
    /// alone, acked as soon as it has been written to the socket.
    Next {
        room: String,
        identity: String,
        timeout_secs: u64,
        #[serde(default)]
        manual_ack: bool,
        /// Seconds; default from config (600).
        #[serde(default)]
        lease_secs: Option<u64>,
    },
    /// Look at messages from the bookmark (or from `since`). A pure view:
    /// it moves nothing, unless `ack` settles exactly what it returned.
    Read {
        room: String,
        identity: String,
        since: Option<u64>,
        limit: u32,
        #[serde(default)]
        ack: bool,
    },
    /// Settle a delivery: taken on. Not "finished": that is a `done` in
    /// the room.
    Ack {
        identity: String,
        token: String,
    },
    /// Still working on it: keep it this reader's for longer.
    Renew {
        identity: String,
        token: String,
        #[serde(default)]
        lease_secs: Option<u64>,
    },
    /// Not now: hand it out again after `retry_in_secs` (default 60).
    Nack {
        identity: String,
        token: String,
        #[serde(default)]
        retry_in_secs: Option<u64>,
    },
    /// What is owed to this identity, without handing anything out. For
    /// wake-up hooks.
    Peek {
        room: String,
        identity: String,
        limit: u32,
    },
    /// Messages handed out and not simply done: leased, delayed,
    /// quarantined. For this identity, or all local ones with `all`.
    Deliveries {
        room: String,
        identity: String,
        #[serde(default)]
        all: bool,
    },
    /// Bring a quarantined message back to be handed out again.
    Replay {
        room: String,
        identity: String,
        seq: u64,
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
    /// exactly this action, and the room's home records its spend for this
    /// operation. `identity` is the member acting; `op_id` names the
    /// operation, the same on every retry of it (made up if not given).
    CheckApprove {
        room: String,
        action: diavlos_core::Action,
        #[serde(default = "default_identity")]
        identity: String,
        #[serde(default)]
        op_id: Option<String>,
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
    /// Streaming: messages from the bookmark onward, then live. With
    /// `manual_ack` each line carries a lease token and the next line
    /// waits until that one is settled (or its lease runs out). Without
    /// it (older callers) each is acked once written to the socket.
    Watch {
        room: String,
        identity: String,
        #[serde(default)]
        manual_ack: bool,
    },
    /// Who a local identity label is: its name, kind and key. Makes the key
    /// on first use, as every other request that names an identity does.
    Whoami {
        identity: String,
    },
    /// Messages still to reach a room's home, and ones it would not take.
    Outbox {
        #[serde(default)]
        room: Option<String>,
    },
    /// Put a failed or quarantined message back in line, unchanged.
    OutboxRetry {
        id: String,
    },
    /// Give up on a failed or quarantined message. Its content is wiped.
    OutboxDrop {
        id: String,
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
    /// Still to send: pending or waiting to try again.
    pub queued: u64,
    /// Messages handed out too many times and never settled, for the
    /// identities on this helper; see `diavlos deliveries`.
    #[serde(default)]
    pub quarantined_deliveries: u64,
    /// The home refused them for good; see `diavlos outbox`.
    #[serde(default)]
    pub failed: u64,
    /// The same unknown answer kept coming back; see `diavlos outbox`.
    #[serde(default)]
    pub quarantined: u64,
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
    /// The operation the spend is recorded for. Deduplicate on it: a
    /// spend is permission for one operation, not proof it ran once.
    #[serde(default)]
    pub op_id: String,
    /// When the home recorded the spend, and where its audit event is.
    #[serde(default)]
    pub spent_at: String,
    #[serde(default)]
    pub audit_seq: u64,
}

fn default_identity() -> String {
    "default".into()
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

/// One message in the outbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxItem {
    pub id: String,
    pub room: String,
    pub sender: String,
    /// pending, waiting, failed, quarantined or dropped.
    pub state: String,
    #[serde(default)]
    pub kind: Option<MessageType>,
    /// The first line of the text, shortened. Empty once dropped.
    #[serde(default)]
    pub text: String,
    pub created: String,
    pub attempts: u32,
    #[serde(default)]
    pub retry_at: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// transport, paused, budget, home, refused or unknown.
    #[serde(default)]
    pub reason_class: Option<String>,
    #[serde(default)]
    pub code: Option<i32>,
}

/// A message handed out to one reader, and how to settle it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryInfo {
    /// Pass to ack, renew or nack. Replaced each time the message is
    /// handed out, so a worker whose lease ran out cannot settle a newer
    /// delivery.
    pub token: String,
    pub lease_until: String,
    /// 1 the first time; more means it was handed out before and not
    /// settled.
    pub attempt: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextResult {
    pub message: Message,
    pub delivery: DeliveryInfo,
}

/// One entry of `deliveries`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryItem {
    pub seq: u64,
    pub reader: String,
    /// leased, delayed, quarantined or replay.
    pub state: String,
    pub attempt: u32,
    #[serde(default)]
    pub lease_until: Option<String>,
    #[serde(default)]
    pub retry_at: Option<String>,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub kind: Option<MessageType>,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadResult {
    pub messages: Vec<Message>,
    pub bookmark: u64,
}
