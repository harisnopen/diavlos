//! What a message looks like.
//!
//! Every message is a small JSON object. Agents get it as-is. Humans see
//! just the text. The chain is over envelopes; content (text, action, data)
//! sits beside it, so a delete leaves a tombstone and the chain still proves
//! nothing else changed.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::canonical::{canonical_json, sha256_json};
use crate::error::{Error, Result};
use crate::keys::{PublicKey, Signer};

/// Schema version. Lets the format grow without breaking old helpers.
pub const SCHEMA_VERSION: u32 = 1;

/// Biggest message we store or relay, in bytes of JSON. Big things go by
/// reference: hash, size, where. Never inline.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// An approve stops counting after this many seconds. Timeout means no.
pub const APPROVE_TTL_SECS: i64 = 600;

/// `prev` of the first message in a room.
pub const GENESIS_PREV: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

/// The type of a message. An agent knows what it got without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageType {
    /// Just talking. Anyone.
    Chat,
    /// Please do this. Agent or human.
    Task,
    /// I need an answer before I go on. Agent. (`diavlos ask` sends this.)
    #[serde(alias = "ask")]
    Question,
    /// Answer to a question or task (reply_to set). Agent.
    Reply,
    /// Task finished, here's the result. Agent.
    Done,
    /// I'm taking this task. Agent.
    Claim,
    /// I'm giving this task back. Agent.
    Release,
    /// A human says yes to a risky action. Human key only.
    Approve,
    /// A human says no, with a reason. Human key only.
    Deny,
    /// pause, mute, revoke, grant. Owner key only.
    Control,
    /// Joined, left, name taken. Helper.
    System,
}

impl MessageType {
    pub const ALL: [MessageType; 11] = [
        MessageType::Chat,
        MessageType::Task,
        MessageType::Question,
        MessageType::Reply,
        MessageType::Done,
        MessageType::Claim,
        MessageType::Release,
        MessageType::Approve,
        MessageType::Deny,
        MessageType::Control,
        MessageType::System,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            MessageType::Chat => "chat",
            MessageType::Task => "task",
            MessageType::Question => "question",
            MessageType::Reply => "reply",
            MessageType::Done => "done",
            MessageType::Claim => "claim",
            MessageType::Release => "release",
            MessageType::Approve => "approve",
            MessageType::Deny => "deny",
            MessageType::Control => "control",
            MessageType::System => "system",
        }
    }
}

impl fmt::Display for MessageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MessageType {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "chat" => Ok(MessageType::Chat),
            "task" => Ok(MessageType::Task),
            "question" | "ask" => Ok(MessageType::Question),
            "reply" => Ok(MessageType::Reply),
            "done" => Ok(MessageType::Done),
            "claim" => Ok(MessageType::Claim),
            "release" => Ok(MessageType::Release),
            "approve" => Ok(MessageType::Approve),
            "deny" => Ok(MessageType::Deny),
            "control" => Ok(MessageType::Control),
            "system" => Ok(MessageType::System),
            other => Err(Error::Invalid(format!("unknown message type: {other}"))),
        }
    }
}

/// Data class. The helper can refuse to store or relay based on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DataClass {
    Public,
    #[default]
    Internal,
    Confidential,
    Pii,
}

impl DataClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            DataClass::Public => "public",
            DataClass::Internal => "internal",
            DataClass::Confidential => "confidential",
            DataClass::Pii => "pii",
        }
    }
}

impl fmt::Display for DataClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DataClass {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "public" => Ok(DataClass::Public),
            "internal" => Ok(DataClass::Internal),
            "confidential" => Ok(DataClass::Confidential),
            "pii" => Ok(DataClass::Pii),
            other => Err(Error::Invalid(format!("unknown data class: {other}"))),
        }
    }
}

/// Which vendor, model, and owning human. "Which LLM touched this?"
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// Structured intent for a question that needs an approve. The approve signs
/// its hash, so the agent can't ask for v1.2 and ship v1.3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    pub verb: String,
    pub target: String,
    #[serde(default)]
    pub params: Value,
}

impl Action {
    /// `sha256:<hex>` of the canonical action.
    pub fn hash(&self) -> String {
        sha256_json(&serde_json::to_value(self).expect("action serializes"))
    }
}

/// The content that sits beside the envelope. This is what a GDPR delete
/// removes. The envelope keeps its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Content {
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
    #[serde(default)]
    pub data: Value,
}

impl Content {
    pub fn hash(&self) -> String {
        sha256_json(&serde_json::to_value(self).expect("content serializes"))
    }
}

/// A message. Exactly the JSON an agent sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub v: u32,
    pub id: String,
    pub room: String,
    /// Sequence number in the room. Assigned by the room's home helper.
    #[serde(default)]
    pub seq: u64,
    /// Hash of the message before it. Assigned with `seq`.
    #[serde(default)]
    pub prev: String,
    #[serde(default)]
    pub trace: Option<String>,
    pub from: String,
    #[serde(default)]
    pub agent: Option<AgentInfo>,
    #[serde(rename = "type")]
    pub kind: MessageType,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub action: Option<Action>,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub class: DataClass,
    pub ts: String,
    /// On an approve: hash of the action being approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_hash: Option<String>,
    /// On an approve: when it stops counting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
    /// On an approve: it works once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub once: Option<bool>,
    #[serde(default)]
    pub sig: String,
    /// Hash of the content as it was when signed. Only present when the
    /// content is gone (a tombstone) or in an audit bundle, so the chain
    /// and the signature can still be checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// True when the content was deleted and only the envelope remains.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tombstone: bool,
}

/// Everything you need to build a new message. `seq`, `prev`, `sig`, `id`
/// and `ts` are filled in by [`Message::new`].
#[derive(Debug, Clone, Default)]
pub struct Draft {
    pub room: String,
    pub from: String,
    pub kind: Option<MessageType>,
    pub text: String,
    pub action: Option<Action>,
    pub data: Value,
    pub reply_to: Option<String>,
    pub to: Option<String>,
    pub trace: Option<String>,
    pub class: Option<DataClass>,
    pub agent: Option<AgentInfo>,
    pub action_hash: Option<String>,
    pub expires: Option<String>,
    pub once: Option<bool>,
}

/// A fresh, unique, sortable message id.
pub fn new_id() -> String {
    format!("m_{}", ulid::Ulid::generate())
}

/// Now, as RFC 3339 in UTC with second precision.
/// How far ahead of the home's clock a message's `ts` may be.
pub const MAX_CLOCK_SKEW_SECS: i64 = 300;

pub fn now_ts() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Message {
    /// Build and sign a message. It has no `seq` or `prev` yet; the room's
    /// home helper assigns those with [`Message::sequence`].
    pub fn new(draft: Draft, signer: &dyn Signer) -> Result<Self> {
        let mut m = Message {
            v: SCHEMA_VERSION,
            id: new_id(),
            room: draft.room,
            seq: 0,
            prev: String::new(),
            trace: draft.trace,
            from: draft.from,
            agent: draft.agent,
            kind: draft.kind.unwrap_or(MessageType::Chat),
            text: draft.text,
            action: draft.action,
            data: if draft.data.is_null() {
                Value::Null
            } else {
                draft.data
            },
            reply_to: draft.reply_to,
            to: draft.to,
            class: draft.class.unwrap_or_default(),
            ts: now_ts(),
            action_hash: draft.action_hash,
            expires: draft.expires,
            once: draft.once,
            sig: String::new(),
            content_hash: None,
            tombstone: false,
        };
        m.sig = signer.sign(&m.signing_bytes());
        m.check_size()?;
        Ok(m)
    }

    /// The content beside the envelope.
    pub fn content(&self) -> Content {
        Content {
            text: self.text.clone(),
            action: self.action.clone(),
            data: self.data.clone(),
        }
    }

    /// `sha256:<hex>` of the content as signed. Uses the stored hash when
    /// the content is gone.
    pub fn content_hash(&self) -> String {
        match &self.content_hash {
            Some(h) => h.clone(),
            None => self.content().hash(),
        }
    }

    /// A copy that carries its content hash explicitly, for bundles.
    pub fn with_explicit_hash(&self) -> Message {
        let mut m = self.clone();
        m.content_hash = Some(self.content_hash());
        m
    }

    /// The bytes the sender signs. Everything except `seq`, `prev` and
    /// `sig`, with the content replaced by its hash.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let v = serde_json::json!({
            "v": self.v,
            "id": self.id,
            "room": self.room,
            "trace": self.trace,
            "from": self.from,
            "agent": self.agent,
            "type": self.kind,
            "content_hash": self.content_hash(),
            "reply_to": self.reply_to,
            "to": self.to,
            "class": self.class,
            "ts": self.ts,
            "action_hash": self.action_hash,
            "expires": self.expires,
            "once": self.once,
        });
        canonical_json(&v).into_bytes()
    }

    /// Check the sender's signature against a key.
    pub fn verify(&self, key: &PublicKey) -> Result<()> {
        key.verify(&self.signing_bytes(), &self.sig)
    }

    /// Check `ts` as the home sees it on arrival: the exact format the spec
    /// asks for (RFC 3339 UTC, whole seconds, `Z`), which is what makes the
    /// string comparisons in expiry and retention sound, and not more than
    /// `MAX_CLOCK_SKEW_SECS` ahead of `now`, so a sender cannot date a
    /// message into the future to dodge retention or stretch an approve.
    /// A past `ts` is fine: a message can wait days in an outbox.
    pub fn check_ts(&self, now: chrono::DateTime<chrono::Utc>) -> Result<()> {
        let t = chrono::DateTime::parse_from_rfc3339(&self.ts)
            .map_err(|_| Error::Invalid(format!("ts {:?} is not RFC 3339", self.ts)))?
            .with_timezone(&chrono::Utc);
        if t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true) != self.ts {
            return Err(Error::Invalid(format!(
                "ts {:?} must be UTC, whole seconds, ending in Z",
                self.ts
            )));
        }
        if t > now + chrono::Duration::seconds(MAX_CLOCK_SKEW_SECS) {
            return Err(Error::Invalid(format!(
                "ts {} is in the future; check this computer's clock",
                self.ts
            )));
        }
        Ok(())
    }

    /// The envelope: everything except the content, plus the content hash.
    /// This is what the chain is over.
    pub fn envelope(&self) -> Value {
        serde_json::json!({
            "v": self.v,
            "id": self.id,
            "room": self.room,
            "seq": self.seq,
            "prev": self.prev,
            "trace": self.trace,
            "from": self.from,
            "agent": self.agent,
            "type": self.kind,
            "content_hash": self.content_hash(),
            "reply_to": self.reply_to,
            "to": self.to,
            "class": self.class,
            "ts": self.ts,
            "action_hash": self.action_hash,
            "expires": self.expires,
            "once": self.once,
            "sig": self.sig,
        })
    }

    /// `sha256:<hex>` of the envelope. The next message's `prev`.
    pub fn chain_hash(&self) -> String {
        sha256_json(&self.envelope())
    }

    /// Assign a place in the room's chain.
    pub fn sequence(&mut self, seq: u64, prev: &str) {
        self.seq = seq;
        self.prev = prev.to_string();
    }

    /// True once `sequence` has been called.
    pub fn is_sequenced(&self) -> bool {
        self.seq > 0 && !self.prev.is_empty()
    }

    /// Reject messages over the size cap.
    pub fn check_size(&self) -> Result<()> {
        let len = serde_json::to_string(self)?.len();
        if len > MAX_MESSAGE_BYTES {
            return Err(Error::Invalid(format!(
                "message is {len} bytes, cap is {MAX_MESSAGE_BYTES}. Send big things by reference."
            )));
        }
        Ok(())
    }

    /// The message with content removed: a tombstone. The envelope, and so
    /// the chain and the signature, are unchanged.
    pub fn tombstone(&self) -> Message {
        let mut t = self.clone();
        t.content_hash = Some(self.content_hash());
        t.tombstone = true;
        t.text = String::new();
        t.action = None;
        t.data = Value::Null;
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{Identity, Kind};

    fn draft(room: &str, from: &str, text: &str) -> Draft {
        Draft {
            room: room.into(),
            from: from.into(),
            text: text.into(),
            kind: Some(MessageType::Task),
            ..Default::default()
        }
    }

    #[test]
    fn new_message_is_signed_and_verifies() {
        let alice = Identity::generate("alice", Kind::Agent);
        let m = Message::new(draft("ops", "alice", "deploy?"), &alice).unwrap();
        assert_eq!(m.v, 1);
        assert!(m.id.starts_with("m_"));
        m.verify(&alice.public()).unwrap();
        let bob = Identity::generate("bob", Kind::Agent);
        assert!(m.verify(&bob.public()).is_err());
    }

    #[test]
    fn ts_must_be_well_formed_and_not_in_the_future() {
        let alice = Identity::generate("alice", Kind::Agent);
        let mut m = Message::new(draft("ops", "alice", "hi"), &alice).unwrap();
        let now = chrono::Utc::now();
        m.check_ts(now).unwrap();
        // Old is fine: it may have waited in an outbox.
        m.ts = "2020-01-01T00:00:00Z".into();
        m.check_ts(now).unwrap();
        // A little clock skew is fine; far ahead is not.
        let soon = now + chrono::Duration::seconds(60);
        m.ts = soon.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        m.check_ts(now).unwrap();
        m.ts = "2999-01-01T00:00:00Z".into();
        assert!(m.check_ts(now).is_err());
        // Anything but the one format would break string comparisons.
        for bad in [
            "2020-01-01T00:00:00+00:00",
            "2020-01-01T00:00:00.5Z",
            "2020-01-01 00:00:00Z",
            "yesterday",
        ] {
            m.ts = bad.into();
            assert!(m.check_ts(now).is_err(), "{bad}");
        }
    }

    #[test]
    fn sequencing_does_not_break_signature() {
        let alice = Identity::generate("alice", Kind::Agent);
        let mut m = Message::new(draft("ops", "alice", "hi"), &alice).unwrap();
        m.sequence(1, GENESIS_PREV);
        m.verify(&alice.public()).unwrap();
        assert!(m.is_sequenced());
    }

    #[test]
    fn editing_content_breaks_signature() {
        let alice = Identity::generate("alice", Kind::Agent);
        let mut m = Message::new(draft("ops", "alice", "ship v1.2"), &alice).unwrap();
        m.text = "ship v1.3".into();
        assert!(m.verify(&alice.public()).is_err());
    }

    #[test]
    fn chain_links_and_survives_tombstone() {
        let alice = Identity::generate("alice", Kind::Agent);
        let mut a = Message::new(draft("ops", "alice", "first"), &alice).unwrap();
        a.sequence(1, GENESIS_PREV);
        let mut b = Message::new(draft("ops", "alice", "second"), &alice).unwrap();
        b.sequence(2, &a.chain_hash());
        assert_eq!(b.prev, a.chain_hash());
        // Delete the content of `a`: the chain hash and the signature must
        // not change, because the tombstone keeps the content hash.
        let t = a.tombstone();
        assert_eq!(t.text, "");
        assert!(t.tombstone);
        assert_eq!(t.content_hash(), a.content_hash());
        assert_eq!(t.chain_hash(), b.prev);
        t.verify(&alice.public()).unwrap();
        let v = serde_json::to_value(&t).unwrap();
        assert!(v.get("content_hash").is_some());
        let v = serde_json::to_value(&a).unwrap();
        assert!(v.get("content_hash").is_none());
        assert!(v.get("tombstone").is_none());
    }

    #[test]
    fn json_shape_matches_plan() {
        let alice = Identity::generate("alice", Kind::Agent);
        let mut d = draft("ops", "alice", "Deploy api-service v1.2 to prod?");
        d.kind = Some(MessageType::Question);
        d.trace = Some("ticket-4711".into());
        d.action = Some(Action {
            verb: "deploy".into(),
            target: "api-service".into(),
            params: serde_json::json!({"version": "1.2", "env": "prod"}),
        });
        let m = Message::new(d, &alice).unwrap();
        let v = serde_json::to_value(&m).unwrap();
        for key in [
            "v", "id", "room", "seq", "prev", "trace", "from", "agent", "type", "text", "action",
            "data", "reply_to", "to", "class", "ts", "sig",
        ] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
        assert_eq!(v["type"], "question");
        assert_eq!(v["class"], "internal");
        assert!(v.get("action_hash").is_none());
        // Round trip.
        let back: Message = serde_json::from_value(v).unwrap();
        assert_eq!(back, m);
        // "ask" is accepted as an alias for question.
        let asked: MessageType = serde_json::from_str("\"ask\"").unwrap();
        assert_eq!(asked, MessageType::Question);
    }

    #[test]
    fn size_cap() {
        let alice = Identity::generate("alice", Kind::Agent);
        let big = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert!(Message::new(draft("ops", "alice", &big), &alice).is_err());
    }
}
