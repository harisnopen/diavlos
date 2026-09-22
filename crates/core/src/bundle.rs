//! Signed audit bundles. `export` makes one; `verify` checks it with no
//! helper running. For auditors.
//!
//! Format: JSON lines. Line one is the signed header. Every other line is
//! one message, with its content hash made explicit so tombstones verify.

use serde::{Deserialize, Serialize};

use crate::canonical::{canonical_json, sha256_bytes};
use crate::error::{Error, Result};
use crate::keys::{PublicKey, Signer};
use crate::message::{Message, MessageType};
use crate::room::{Member, Room};

pub const BUNDLE_FORMAT: &str = "diavlos-bundle/1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleMember {
    pub name: String,
    pub key: PublicKey,
    pub kind: crate::keys::Kind,
    pub role: crate::room::Role,
    pub joined_at: String,
    #[serde(default)]
    pub revoked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleRoom {
    pub id: String,
    pub name: String,
    pub about: String,
    pub owner: PublicKey,
    pub created: String,
    pub class: crate::message::DataClass,
    #[serde(default)]
    pub retention_days: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    pub bundle: String,
    pub room: BundleRoom,
    pub members: Vec<BundleMember>,
    pub exported_by: String,
    pub exported_by_key: PublicKey,
    pub exported_at: String,
    #[serde(default)]
    pub since: Option<String>,
    pub count: u64,
    pub first_seq: u64,
    pub last_seq: u64,
    /// sha256 over the chain hashes of every message, in order.
    pub messages_hash: String,
    pub sig: String,
}

impl Header {
    fn signing_bytes(&self) -> Vec<u8> {
        let mut v = serde_json::to_value(self).expect("header serializes");
        v.as_object_mut().expect("object").remove("sig");
        canonical_json(&v).into_bytes()
    }
}

fn messages_hash(messages: &[Message]) -> String {
    let mut all = String::new();
    for m in messages {
        all.push_str(&m.chain_hash());
        all.push('\n');
    }
    sha256_bytes(all.as_bytes())
}

/// Build a bundle. Returns the JSONL text.
pub fn export(
    room: &Room,
    members: &[Member],
    messages: &[Message],
    exporter_name: &str,
    exporter: &dyn Signer,
    since: Option<&str>,
) -> Result<String> {
    let messages: Vec<Message> = messages.iter().map(Message::with_explicit_hash).collect();
    let mut header = Header {
        bundle: BUNDLE_FORMAT.into(),
        room: BundleRoom {
            id: room.id.clone(),
            name: room.name.clone(),
            about: room.about.clone(),
            owner: room.owner,
            created: room.created.clone(),
            class: room.class,
            retention_days: room.retention_days,
        },
        members: members
            .iter()
            .map(|m| BundleMember {
                name: m.name.clone(),
                key: m.key,
                kind: m.kind,
                role: m.role,
                joined_at: m.joined_at.clone(),
                revoked: m.revoked,
            })
            .collect(),
        exported_by: exporter_name.to_string(),
        exported_by_key: exporter.public(),
        exported_at: crate::message::now_ts(),
        since: since.map(String::from),
        count: messages.len() as u64,
        first_seq: messages.first().map(|m| m.seq).unwrap_or(0),
        last_seq: messages.last().map(|m| m.seq).unwrap_or(0),
        messages_hash: messages_hash(&messages),
        sig: String::new(),
    };
    header.sig = exporter.sign(&header.signing_bytes());
    let mut out = serde_json::to_string(&header)?;
    out.push('\n');
    for m in &messages {
        out.push_str(&serde_json::to_string(m)?);
        out.push('\n');
    }
    Ok(out)
}

/// What `verify` found.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub room: String,
    pub room_id: String,
    pub exported_by: String,
    pub exported_at: String,
    pub count: u64,
    pub first_seq: u64,
    pub last_seq: u64,
    pub tombstones: u64,
    pub chain_from_genesis: bool,
    /// Fingerprint of the owner key the bundle claims. A bundle only proves
    /// it is whole and self-consistent; anyone can make a fresh key and a
    /// room around it. Compare this with the fingerprint `diavlos who`
    /// shows for the real owner, or pin it with `verify --owner`.
    #[serde(default)]
    pub owner_fingerprint: String,
    pub problems: Vec<String>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Check a bundle standalone. Signatures, the chain, membership.
pub fn verify(text: &str) -> Result<Report> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header_line = lines
        .next()
        .ok_or_else(|| Error::Invalid("empty bundle".into()))?;
    let header: Header = serde_json::from_str(header_line)
        .map_err(|e| Error::Invalid(format!("bad bundle header: {e}")))?;
    if header.bundle != BUNDLE_FORMAT {
        return Err(Error::Invalid(format!(
            "unknown bundle format {}",
            header.bundle
        )));
    }
    let mut report = Report {
        room: header.room.name.clone(),
        room_id: header.room.id.clone(),
        exported_by: header.exported_by.clone(),
        exported_at: header.exported_at.clone(),
        count: header.count,
        first_seq: header.first_seq,
        last_seq: header.last_seq,
        owner_fingerprint: header.room.owner.fingerprint(),
        ..Default::default()
    };
    let mut problems = Vec::new();

    if header
        .exported_by_key
        .verify(&header.signing_bytes(), &header.sig)
        .is_err()
    {
        problems.push("header signature does not verify".into());
    }
    if !header
        .members
        .iter()
        .any(|m| m.key == header.exported_by_key)
    {
        problems.push("exporter key is not in the member list".into());
    }

    let mut messages = Vec::new();
    for (i, line) in lines.enumerate() {
        match serde_json::from_str::<Message>(line) {
            Ok(m) => messages.push(m),
            Err(e) => problems.push(format!("line {} is not a message: {e}", i + 2)),
        }
    }
    if messages.len() as u64 != header.count {
        problems.push(format!(
            "header says {} messages, bundle has {}",
            header.count,
            messages.len()
        ));
    }
    if messages_hash(&messages) != header.messages_hash {
        problems.push("messages hash does not match the header".into());
    }

    // Membership: from the log itself when it starts at genesis, else from
    // the signed header.
    let from_genesis = messages.first().map(|m| m.seq == 1).unwrap_or(false);
    report.chain_from_genesis = from_genesis;
    let mut keys: std::collections::HashMap<String, PublicKey> = if from_genesis {
        Default::default()
    } else {
        header
            .members
            .iter()
            .map(|m| (m.name.clone(), m.key))
            .collect()
    };
    if from_genesis {
        // The owner signs the first message and every joined notice.
        if let Some(first) = messages.first() {
            keys.insert(first.from.clone(), header.room.owner);
        }
    }

    let mut prev_hash: Option<String> = None;
    let mut prev_seq: Option<u64> = None;
    for m in &messages {
        if m.room != header.room.id {
            problems.push(format!("message {} is for another room", m.id));
        }
        if let Some(ps) = prev_seq {
            if m.seq != ps + 1 {
                problems.push(format!("gap in the chain before seq {}", m.seq));
            }
        }
        if let Some(ph) = &prev_hash {
            if &m.prev != ph {
                problems.push(format!("chain break at seq {}: prev does not match", m.seq));
            }
        } else if from_genesis && m.prev != crate::message::GENESIS_PREV {
            problems.push("first message does not start the chain".into());
        }
        if m.tombstone {
            report.tombstones += 1;
            // Erased means erased: the explicit content_hash would let
            // edited text ride along unchecked.
            if !m.text.is_empty() || m.action.is_some() || !m.data.is_null() {
                problems.push(format!(
                    "seq {} is a tombstone but still carries content",
                    m.seq
                ));
            }
        } else if m.content().hash() != m.content_hash() {
            problems.push(format!("content of seq {} does not match its hash", m.seq));
        }
        match keys.get(&m.from) {
            Some(key) => {
                if m.verify(key).is_err() {
                    problems.push(format!("bad signature on seq {} from {}", m.seq, m.from));
                }
                if matches!(m.kind, MessageType::System | MessageType::Control)
                    && *key != header.room.owner
                {
                    problems.push(format!(
                        "seq {} is a {} message not signed by the owner",
                        m.seq, m.kind
                    ));
                }
            }
            None => problems.push(format!("seq {} from unknown member {}", m.seq, m.from)),
        }
        // Learn members from joined notices signed by the owner.
        if m.kind == MessageType::System && keys.get(&m.from) == Some(&header.room.owner) {
            if let Some(member) = m.data.get("member") {
                if let (Some(name), Some(key)) = (
                    member.get("name").and_then(|v| v.as_str()),
                    member.get("key").and_then(|v| v.as_str()),
                ) {
                    if let Ok(k) = key.parse::<PublicKey>() {
                        keys.entry(name.to_string()).or_insert(k);
                    }
                }
            }
        }
        prev_hash = Some(m.chain_hash());
        prev_seq = Some(m.seq);
    }
    report.problems = problems;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{Identity, Kind};
    use crate::message::{DataClass, Draft, GENESIS_PREV};
    use crate::room::Role;

    fn setup() -> (Identity, Identity, Room, Vec<Member>, Vec<Message>) {
        let owner = Identity::generate("haris", Kind::Human);
        let bob = Identity::generate("bob", Kind::Agent);
        let room = Room {
            id: "r_test".into(),
            name: "ops".into(),
            about: "".into(),
            owner: owner.public(),
            created: "2026-01-01T00:00:00Z".into(),
            retention_days: None,
            class: DataClass::Internal,
            paused: false,
            hold: false,
            closed: false,
            home_node: "n".into(),
            home_hints: serde_json::Value::Null,
        };
        let mk = |name: &str, id: &Identity, kind: Kind, role: Role| Member {
            room_id: "r_test".into(),
            name: name.into(),
            key: id.public(),
            kind,
            role,
            node: None,
            granted_by: owner.public(),
            expires_at: None,
            joined_at: "2026-01-01T00:00:00Z".into(),
            last_seen: None,
            muted: false,
            revoked: false,
            profile: serde_json::Value::Null,
        };
        let members = vec![
            mk("haris", &owner, Kind::Human, Role::Approver),
            mk("bob", &bob, Kind::Agent, Role::TaskGiver),
        ];
        let mut msgs = Vec::new();
        let mut m1 = Message::new(
            Draft {
                room: "r_test".into(),
                from: "haris".into(),
                kind: Some(MessageType::System),
                text: "made".into(),
                data: serde_json::json!({"event":"joined","member": members[0]}),
                ..Default::default()
            },
            &owner,
        )
        .unwrap();
        m1.sequence(1, GENESIS_PREV);
        let mut m2 = Message::new(
            Draft {
                room: "r_test".into(),
                from: "haris".into(),
                kind: Some(MessageType::System),
                text: "bob joined".into(),
                data: serde_json::json!({"event":"joined","member": members[1]}),
                ..Default::default()
            },
            &owner,
        )
        .unwrap();
        m2.sequence(2, &m1.chain_hash());
        let mut m3 = Message::new(
            Draft {
                room: "r_test".into(),
                from: "bob".into(),
                kind: Some(MessageType::Task),
                text: "fix it".into(),
                ..Default::default()
            },
            &bob,
        )
        .unwrap();
        m3.sequence(3, &m2.chain_hash());
        msgs.push(m1);
        msgs.push(m2);
        msgs.push(m3);
        (owner, bob, room, members, msgs)
    }

    #[test]
    fn export_then_verify_passes() {
        let (owner, _bob, room, members, msgs) = setup();
        let text = export(&room, &members, &msgs, "haris", &owner, None).unwrap();
        let report = verify(&text).unwrap();
        assert!(report.ok(), "{:?}", report.problems);
        assert_eq!(report.count, 3);
        assert!(report.chain_from_genesis);
    }

    #[test]
    fn tombstones_still_verify() {
        let (owner, _bob, room, members, mut msgs) = setup();
        msgs[2] = msgs[2].tombstone();
        let text = export(&room, &members, &msgs, "haris", &owner, None).unwrap();
        let report = verify(&text).unwrap();
        assert!(report.ok(), "{:?}", report.problems);
        assert_eq!(report.tombstones, 1);
    }

    #[test]
    fn a_tombstone_that_still_carries_text_is_caught() {
        let (owner, _bob, room, members, mut msgs) = setup();
        // The exporter signs the header, so a dishonest one can make the
        // hashes line up. The tombstone itself must still be empty.
        msgs[2] = msgs[2].tombstone();
        msgs[2].text = "something else".into();
        let text = export(&room, &members, &msgs, "haris", &owner, None).unwrap();
        let r = verify(&text).unwrap();
        assert!(
            r.problems
                .iter()
                .any(|p| p.contains("still carries content")),
            "{:?}",
            r.problems
        );
    }

    #[test]
    fn the_report_names_the_owner_key() {
        let (owner, _bob, room, members, msgs) = setup();
        let text = export(&room, &members, &msgs, "haris", &owner, None).unwrap();
        let r = verify(&text).unwrap();
        assert_eq!(r.owner_fingerprint, owner.public().fingerprint());
    }

    #[test]
    fn tampering_is_caught() {
        let (owner, _bob, room, members, msgs) = setup();
        let text = export(&room, &members, &msgs, "haris", &owner, None).unwrap();
        let tampered = text.replace("fix it", "rm -rf");
        let report = verify(&tampered).unwrap();
        assert!(!report.ok());
        assert!(report
            .problems
            .iter()
            .any(|p| p.contains("does not match its hash")));
        // Drop a line: count and hash mismatch.
        let mut lines: Vec<&str> = text.lines().collect();
        lines.remove(2);
        let shorter = lines.join("\n");
        let report = verify(&shorter).unwrap();
        assert!(!report.ok());
    }

    #[test]
    fn partial_bundle_uses_header_members() {
        let (owner, _bob, room, members, msgs) = setup();
        let text = export(
            &room,
            &members,
            &msgs[2..],
            "haris",
            &owner,
            Some("2026-01-02"),
        )
        .unwrap();
        let report = verify(&text).unwrap();
        assert!(report.ok(), "{:?}", report.problems);
        assert!(!report.chain_from_genesis);
        assert_eq!(report.first_seq, 3);
    }
}
