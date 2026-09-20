//! Control messages: pause, mute, revoke, grant. Owner key only. Changes
//! who may do what. Signed, instant, everyone obeys.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::message::{Message, MessageType};
use crate::room::Role;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum ControlOp {
    /// Give a member a role. Can expire.
    Grant {
        name: String,
        role: Role,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<String>,
    },
    /// Kill switch. Nothing moves until resume.
    Pause,
    Resume,
    /// Silence one member.
    Mute {
        name: String,
    },
    Unmute {
        name: String,
    },
    /// Cut one key for good. A signed blocklist entry.
    Revoke {
        name: String,
    },
    /// Legal hold. Retention stops deleting.
    Hold {
        on: bool,
    },
    /// The room was rotated: a new room key. Everyone out.
    Rotated {
        new_room_id: String,
    },
}

impl ControlOp {
    /// Read the op out of a control message.
    pub fn from_message(msg: &Message) -> Result<ControlOp> {
        if msg.kind != MessageType::Control {
            return Err(Error::Invalid("not a control message".into()));
        }
        serde_json::from_value(msg.data.clone())
            .map_err(|e| Error::Invalid(format!("bad control message: {e}")))
    }

    /// One line for the message text.
    pub fn describe(&self) -> String {
        match self {
            ControlOp::Grant { name, role, until } => match until {
                Some(u) => format!("{name} is now {role} until {u}"),
                None => format!("{name} is now {role}"),
            },
            ControlOp::Pause => "room paused".into(),
            ControlOp::Resume => "room resumed".into(),
            ControlOp::Mute { name } => format!("{name} muted"),
            ControlOp::Unmute { name } => format!("{name} unmuted"),
            ControlOp::Revoke { name } => format!("{name} revoked"),
            ControlOp::Hold { on } => {
                if *on {
                    "legal hold on".into()
                } else {
                    "legal hold off".into()
                }
            }
            ControlOp::Rotated { new_room_id } => format!("room rotated to {new_room_id}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let op = ControlOp::Grant {
            name: "bob".into(),
            role: Role::Approver,
            until: Some("2026-12-31T00:00:00Z".into()),
        };
        let v = serde_json::to_value(&op).unwrap();
        assert_eq!(v["op"], "grant");
        assert_eq!(v["role"], "approver");
        let back: ControlOp = serde_json::from_value(v).unwrap();
        assert_eq!(back, op);
        assert_eq!(ControlOp::Pause.describe(), "room paused");
    }
}
