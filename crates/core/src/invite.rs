//! Signed invites.
//!
//! You get into a room with a signed invite from the owner, not a shared
//! password. One invite, one member, used once. A leaked room name alone
//! gets nobody in. The invite carries the peer address so rooms never go on
//! a public phone book. `--for <node-id>` pins it to one machine.

use serde::{Deserialize, Serialize};

use crate::canonical::canonical_json;
use crate::error::{Error, Result};
use crate::keys::{Kind, PublicKey, Signer};
use crate::names::validate_name;
use crate::room::Role;

/// Invite format version.
pub const INVITE_VERSION: u32 = 1;
/// Text prefix on an encoded invite.
pub const INVITE_PREFIX: &str = "dv1.";
/// Invites expire after this many hours by default.
pub const INVITE_TTL_HOURS: i64 = 24;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    pub v: u32,
    pub room_id: String,
    pub room_name: String,
    /// The name this invite binds. Nobody else can take it.
    pub name: String,
    /// human or agent. `--human` marks a key as a person who can approve.
    pub kind: Kind,
    pub role: Role,
    pub owner: PublicKey,
    /// The owner's helper node id. This is where the joiner connects.
    pub home_node: String,
    /// Transport hints for reaching the home node (opaque to core).
    #[serde(default)]
    pub home_hints: serde_json::Value,
    /// Pin to one machine. A leaked invite is then useless anywhere else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_node: Option<String>,
    /// Random, unique. The home helper remembers used ones.
    pub nonce: String,
    pub created: String,
    pub expires: String,
    pub sig: String,
}

/// What `invite` needs to know to make one.
#[derive(Debug, Clone)]
pub struct InviteSpec {
    pub room_id: String,
    pub room_name: String,
    pub name: String,
    pub kind: Kind,
    pub role: Role,
    pub home_node: String,
    pub home_hints: serde_json::Value,
    pub for_node: Option<String>,
    pub ttl_hours: Option<i64>,
}

impl Invite {
    /// Make and sign an invite with the owner's key.
    pub fn create(spec: InviteSpec, owner: &dyn Signer) -> Result<Self> {
        validate_name(&spec.name)?;
        let nonce: [u8; 16] = rand::random();
        let now = chrono::Utc::now();
        let ttl = chrono::Duration::hours(spec.ttl_hours.unwrap_or(INVITE_TTL_HOURS));
        let mut inv = Invite {
            v: INVITE_VERSION,
            room_id: spec.room_id,
            room_name: spec.room_name,
            name: spec.name,
            kind: spec.kind,
            role: spec.role,
            owner: owner.public(),
            home_node: spec.home_node,
            home_hints: spec.home_hints,
            for_node: spec.for_node,
            nonce: format!(
                "i_{}",
                data_encoding::BASE32_NOPAD.encode(&nonce).to_lowercase()
            ),
            created: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            expires: (now + ttl).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            sig: String::new(),
        };
        inv.sig = owner.sign(&inv.signing_bytes());
        Ok(inv)
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut v = serde_json::to_value(self).expect("invite serializes");
        v.as_object_mut().expect("object").remove("sig");
        canonical_json(&v).into_bytes()
    }

    /// Check the owner's signature and the name rule. Does not check expiry
    /// or reuse; the home helper does that against its clock and its list.
    pub fn verify(&self) -> Result<()> {
        if self.v != INVITE_VERSION {
            return Err(Error::Invalid(format!(
                "invite version {} not supported",
                self.v
            )));
        }
        validate_name(&self.name)?;
        validate_name(&self.room_name)?;
        self.owner
            .verify(&self.signing_bytes(), &self.sig)
            .map_err(|_| Error::Denied("invite signature does not match the owner key".into()))
    }

    /// True if the invite is past its expiry at `now` (RFC 3339 UTC).
    pub fn is_expired(&self, now: &str) -> bool {
        self.expires.as_str() <= now
    }

    /// Encode as one pasteable token: `dv1.<base64url>`.
    pub fn encode(&self) -> String {
        let json = serde_json::to_string(self).expect("invite serializes");
        format!(
            "{INVITE_PREFIX}{}",
            data_encoding::BASE64URL_NOPAD.encode(json.as_bytes())
        )
    }

    /// Decode a token. Verifies the signature.
    pub fn decode(token: &str) -> Result<Self> {
        let token = token.trim();
        let body = token
            .strip_prefix(INVITE_PREFIX)
            .ok_or_else(|| Error::Invalid("not a diavlos invite (expected dv1.…)".into()))?;
        let raw = data_encoding::BASE64URL_NOPAD
            .decode(body.as_bytes())
            .map_err(|_| Error::Invalid("invite is not valid base64url".into()))?;
        let inv: Invite = serde_json::from_slice(&raw)
            .map_err(|e| Error::Invalid(format!("invite is not valid: {e}")))?;
        inv.verify()?;
        Ok(inv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Identity;

    fn spec() -> InviteSpec {
        InviteSpec {
            room_id: "r_abc".into(),
            room_name: "ops".into(),
            name: "bob".into(),
            kind: Kind::Agent,
            role: Role::TaskGiver,
            home_node: "node123".into(),
            home_hints: serde_json::json!({"relay": "https://x"}),
            for_node: None,
            ttl_hours: None,
        }
    }

    #[test]
    fn roundtrip_and_verify() {
        let owner = Identity::generate("haris", Kind::Human);
        let inv = Invite::create(spec(), &owner).unwrap();
        let token = inv.encode();
        assert!(token.starts_with("dv1."));
        let back = Invite::decode(&token).unwrap();
        assert_eq!(back, inv);
        assert!(!back.is_expired(&chrono::Utc::now().to_rfc3339()));
    }

    #[test]
    fn tampering_is_caught() {
        let owner = Identity::generate("haris", Kind::Human);
        let mut inv = Invite::create(spec(), &owner).unwrap();
        inv.name = "mallory".into();
        assert!(Invite::decode(&inv.encode()).is_err());
        let mut inv2 = Invite::create(spec(), &owner).unwrap();
        inv2.role = Role::Approver;
        assert!(inv2.verify().is_err());
    }

    #[test]
    fn a_stranger_cannot_forge_one() {
        let owner = Identity::generate("haris", Kind::Human);
        let stranger = Identity::generate("mallory", Kind::Human);
        let mut inv = Invite::create(spec(), &stranger).unwrap();
        inv.owner = owner.public();
        assert!(inv.verify().is_err());
    }

    #[test]
    fn expiry() {
        let owner = Identity::generate("haris", Kind::Human);
        let mut s = spec();
        s.ttl_hours = Some(0);
        let inv = Invite::create(s, &owner).unwrap();
        let later = (chrono::Utc::now() + chrono::Duration::seconds(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        assert!(inv.is_expired(&later));
    }

    #[test]
    fn bad_name_is_refused() {
        let owner = Identity::generate("haris", Kind::Human);
        let mut s = spec();
        s.name = "Bob".into();
        assert!(Invite::create(s, &owner).is_err());
    }
}
