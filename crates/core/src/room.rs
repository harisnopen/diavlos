//! Rooms, members, and roles.
//!
//! A room is made by one key: the owner. Membership is a signed list. Each
//! member has a role that can expire. The owner can pause the room, mute or
//! revoke one member.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::keys::{Kind, PublicKey};
use crate::message::{DataClass, MessageType};

/// What a member may do. Signed grants that can expire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// Read only, for auditors.
    Observer,
    /// May talk, answer, and report done. May not give orders.
    Chat,
    /// May post tasks.
    TaskGiver,
    /// May approve or deny risky actions. Only counts on a human key.
    Approver,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Observer => "observer",
            Role::Chat => "chat",
            Role::TaskGiver => "task-giver",
            Role::Approver => "approver",
        }
    }

    /// Which message types this role may send. The owner may send anything.
    pub fn may_send(&self, kind: MessageType) -> bool {
        use MessageType::*;
        match self {
            Role::Observer => false,
            Role::Chat => matches!(kind, Chat | Question | Reply | Done | Claim | Release),
            Role::TaskGiver => matches!(
                kind,
                Chat | Question | Reply | Done | Claim | Release | Task
            ),
            Role::Approver => matches!(
                kind,
                Chat | Question | Reply | Done | Claim | Release | Task | Approve | Deny
            ),
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Role {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "observer" => Ok(Role::Observer),
            "chat" => Ok(Role::Chat),
            "task-giver" | "task_giver" | "taskgiver" => Ok(Role::TaskGiver),
            "approver" => Ok(Role::Approver),
            other => Err(Error::Invalid(format!("unknown role: {other}"))),
        }
    }
}

/// A room as this helper knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Room {
    /// Random id, stable for the life of the room. `rotate` makes a new one.
    pub id: String,
    /// Human name, unique on this helper.
    pub name: String,
    #[serde(default)]
    pub about: String,
    pub owner: PublicKey,
    pub created: String,
    /// Retention in days. None means keep forever.
    #[serde(default)]
    pub retention_days: Option<u32>,
    /// Data class of the room. Messages default to it.
    #[serde(default)]
    pub class: DataClass,
    #[serde(default)]
    pub paused: bool,
    /// Legal hold. Retention stops deleting.
    #[serde(default)]
    pub hold: bool,
    /// Closed for good (rotated away). Still readable for audit.
    #[serde(default)]
    pub closed: bool,
    /// The node that sequences this room: the owner's helper.
    pub home_node: String,
    /// Transport hints for reaching the home node (opaque to core).
    #[serde(default)]
    pub home_hints: serde_json::Value,
}

/// A fresh random room id.
pub fn new_room_id() -> String {
    let bytes: [u8; 16] = rand::random();
    format!(
        "r_{}",
        data_encoding::BASE32_NOPAD.encode(&bytes).to_lowercase()
    )
}

/// One member of a room. The name is bound to the key at invite time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub room_id: String,
    pub name: String,
    pub key: PublicKey,
    pub kind: Kind,
    pub role: Role,
    /// The helper node this member was last seen from. One key, one machine.
    #[serde(default)]
    pub node: Option<String>,
    pub granted_by: PublicKey,
    #[serde(default)]
    pub expires_at: Option<String>,
    pub joined_at: String,
    #[serde(default)]
    pub last_seen: Option<String>,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub revoked: bool,
    /// Signed profile as presented at join time.
    #[serde(default)]
    pub profile: serde_json::Value,
}

impl Member {
    /// True if the grant has expired at `now` (RFC 3339 strings compare
    /// correctly when both are UTC with the same precision).
    pub fn is_expired(&self, now: &str) -> bool {
        match &self.expires_at {
            Some(exp) => exp.as_str() <= now,
            None => false,
        }
    }

    /// Whether this member may send a message of this type right now.
    pub fn check_may_send(&self, kind: MessageType, is_owner: bool, now: &str) -> Result<()> {
        if self.revoked {
            return Err(Error::Denied(format!("{} was revoked", self.name)));
        }
        if self.muted {
            return Err(Error::Denied(format!("{} is muted", self.name)));
        }
        if self.is_expired(now) {
            return Err(Error::Denied(format!("{}'s role has expired", self.name)));
        }
        // The rule is on the key, not the role: not even the owner may
        // approve with an agent key.
        if matches!(kind, MessageType::Approve | MessageType::Deny) && self.kind != Kind::Human {
            return Err(Error::Denied(format!("only a human key may send {kind}")));
        }
        if is_owner {
            return Ok(());
        }
        if matches!(kind, MessageType::Control | MessageType::System) {
            return Err(Error::Denied(format!("only the owner may send {kind}")));
        }
        if !self.role.may_send(kind) {
            return Err(Error::Denied(format!(
                "{} has role {} and may not send {kind}",
                self.name, self.role
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Identity;

    fn member(kind: Kind, role: Role) -> Member {
        let id = Identity::generate("x", kind);
        Member {
            room_id: "r".into(),
            name: "x".into(),
            key: id.public(),
            kind,
            role,
            node: None,
            granted_by: id.public(),
            expires_at: None,
            joined_at: "2026-01-01T00:00:00Z".into(),
            last_seen: None,
            muted: false,
            revoked: false,
            profile: serde_json::Value::Null,
        }
    }

    #[test]
    fn chat_cannot_give_orders() {
        let m = member(Kind::Agent, Role::Chat);
        assert!(m
            .check_may_send(MessageType::Chat, false, "2026-01-02T00:00:00Z")
            .is_ok());
        assert!(m
            .check_may_send(MessageType::Task, false, "2026-01-02T00:00:00Z")
            .is_err());
    }

    #[test]
    fn approve_needs_human_key() {
        let agent = member(Kind::Agent, Role::Approver);
        assert!(agent
            .check_may_send(MessageType::Approve, false, "2026-01-02T00:00:00Z")
            .is_err());
        let human = member(Kind::Human, Role::Approver);
        assert!(human
            .check_may_send(MessageType::Approve, false, "2026-01-02T00:00:00Z")
            .is_ok());
        // Owning the room does not turn an agent key into a person.
        let owner = member(Kind::Agent, Role::Approver);
        assert!(owner
            .check_may_send(MessageType::Approve, true, "2026-01-02T00:00:00Z")
            .is_err());
        assert!(owner
            .check_may_send(MessageType::Control, true, "2026-01-02T00:00:00Z")
            .is_ok());
    }

    #[test]
    fn expiry_muting_and_revocation() {
        let mut m = member(Kind::Agent, Role::TaskGiver);
        m.expires_at = Some("2026-01-01T12:00:00Z".into());
        assert!(m
            .check_may_send(MessageType::Task, false, "2026-01-01T11:00:00Z")
            .is_ok());
        assert!(m
            .check_may_send(MessageType::Task, false, "2026-01-01T13:00:00Z")
            .is_err());
        m.expires_at = None;
        m.muted = true;
        assert!(m
            .check_may_send(MessageType::Chat, false, "2026-01-01T13:00:00Z")
            .is_err());
        m.muted = false;
        m.revoked = true;
        assert!(m
            .check_may_send(MessageType::Chat, true, "2026-01-01T13:00:00Z")
            .is_err());
    }

    #[test]
    fn role_strings() {
        assert_eq!("task-giver".parse::<Role>().unwrap(), Role::TaskGiver);
        assert_eq!(
            serde_json::to_string(&Role::TaskGiver).unwrap(),
            "\"task-giver\""
        );
    }
}
