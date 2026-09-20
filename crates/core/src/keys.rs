//! Identity: a keypair with a kind, a signed profile, and a claims slot.
//!
//! Every agent, human, and service has one. Every message is signed with it.
//! The signature string carries an algorithm prefix (`ed25519:`) so a
//! different algorithm can be added later without changing the format.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use ed25519_dalek::{Signer as _, SigningKey, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::canonical::canonical_json;
use crate::error::{Error, Result};

/// Algorithm prefix for Ed25519. Keep it: algorithm agility lives here.
pub const ED25519_PREFIX: &str = "ed25519:";

/// What kind of thing holds this key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Human,
    Agent,
    Service,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Kind::Human => write!(f, "human"),
            Kind::Agent => write!(f, "agent"),
            Kind::Service => write!(f, "service"),
        }
    }
}

impl FromStr for Kind {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "human" => Ok(Kind::Human),
            "agent" => Ok(Kind::Agent),
            "service" => Ok(Kind::Service),
            other => Err(Error::Invalid(format!("unknown kind: {other}"))),
        }
    }
}

/// A public key. Serialized as `ed25519:<hex>`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PublicKey([u8; 32]);

impl PublicKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self> {
        VerifyingKey::from_bytes(&bytes)
            .map_err(|e| Error::Invalid(format!("bad public key: {e}")))?;
        Ok(PublicKey(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Short fingerprint shown beside a name: `sha256` of the key, first
    /// 16 hex characters.
    pub fn fingerprint(&self) -> String {
        let digest = Sha256::digest(self.0);
        data_encoding::HEXLOWER.encode(&digest)[..16].to_string()
    }

    /// Verify a signature string (`ed25519:<hex>`) over `msg`.
    pub fn verify(&self, msg: &[u8], sig: &str) -> Result<()> {
        let hex = sig
            .strip_prefix(ED25519_PREFIX)
            .ok_or_else(|| Error::BadSignature("unknown signature algorithm".into()))?;
        let raw = data_encoding::HEXLOWER
            .decode(hex.as_bytes())
            .map_err(|_| Error::BadSignature("signature is not hex".into()))?;
        let raw: [u8; 64] = raw
            .try_into()
            .map_err(|_| Error::BadSignature("signature has wrong length".into()))?;
        let sig = ed25519_dalek::Signature::from_bytes(&raw);
        let vk = VerifyingKey::from_bytes(&self.0)
            .map_err(|e| Error::BadSignature(format!("bad public key: {e}")))?;
        vk.verify(msg, &sig)
            .map_err(|_| Error::BadSignature("signature does not match".into()))
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{ED25519_PREFIX}{}",
            data_encoding::HEXLOWER.encode(&self.0)
        )
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.fingerprint())
    }
}

impl FromStr for PublicKey {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        let hex = s
            .strip_prefix(ED25519_PREFIX)
            .ok_or_else(|| Error::Invalid("public key must start with ed25519:".into()))?;
        let raw = data_encoding::HEXLOWER
            .decode(hex.as_bytes())
            .map_err(|_| Error::Invalid("public key is not hex".into()))?;
        let raw: [u8; 32] = raw
            .try_into()
            .map_err(|_| Error::Invalid("public key has wrong length".into()))?;
        PublicKey::from_bytes(raw)
    }
}

impl Serialize for PublicKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// What an agent says it is. Signed by its own key, shown in `who`,
/// stamped on each message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// The human who owns this agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// A profile plus the key that signed it and the signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedProfile {
    pub key: PublicKey,
    pub kind: Kind,
    pub profile: Profile,
    /// Claims slot. Empty today. Later: "org Acme says this key is alice@acme".
    #[serde(default)]
    pub claims: serde_json::Value,
    pub sig: String,
}

impl SignedProfile {
    fn signing_bytes(
        key: &PublicKey,
        kind: Kind,
        profile: &Profile,
        claims: &serde_json::Value,
    ) -> Vec<u8> {
        let v = serde_json::json!({
            "key": key,
            "kind": kind,
            "profile": profile,
            "claims": claims,
        });
        canonical_json(&v).into_bytes()
    }

    /// Check the profile was signed by the key it names.
    pub fn verify(&self) -> Result<()> {
        let bytes = Self::signing_bytes(&self.key, self.kind, &self.profile, &self.claims);
        self.key.verify(&bytes, &self.sig)
    }
}

/// Anything that can sign. File key today. Keychain, passkey, YubiKey, HSM
/// later. A bank wants a human approve from hardware.
pub trait Signer: Send + Sync {
    fn public(&self) -> PublicKey;
    /// Returns a signature string with an algorithm prefix.
    fn sign(&self, msg: &[u8]) -> String;
}

/// An identity held on this machine: secret key, kind, profile, claims.
#[derive(Clone, Serialize, Deserialize)]
pub struct Identity {
    /// Local label for this key file, and the default member name in rooms.
    pub name: String,
    /// Secret key, hex. Plain text on disk in v0.1. OS keychain in v0.2.
    secret: String,
    pub kind: Kind,
    #[serde(default)]
    pub profile: Profile,
    #[serde(default)]
    pub claims: serde_json::Value,
    #[serde(default = "now_rfc3339")]
    pub created: String,
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("key", &self.public().fingerprint())
            .finish()
    }
}

impl Identity {
    /// Make a new key.
    pub fn generate(name: &str, kind: Kind) -> Self {
        let bytes: [u8; 32] = rand::random();
        let sk = SigningKey::from_bytes(&bytes);
        Identity {
            name: name.to_string(),
            secret: data_encoding::HEXLOWER.encode(&sk.to_bytes()),
            kind,
            profile: Profile::default(),
            claims: serde_json::Value::Object(Default::default()),
            created: now_rfc3339(),
        }
    }

    fn signing_key(&self) -> SigningKey {
        let raw = data_encoding::HEXLOWER
            .decode(self.secret.as_bytes())
            .expect("secret key was written by us as hex");
        let raw: [u8; 32] = raw.try_into().expect("secret key is 32 bytes");
        SigningKey::from_bytes(&raw)
    }

    pub fn public(&self) -> PublicKey {
        PublicKey(self.signing_key().verifying_key().to_bytes())
    }

    /// Sign the profile with this key.
    pub fn signed_profile(&self) -> SignedProfile {
        let key = self.public();
        let bytes = SignedProfile::signing_bytes(&key, self.kind, &self.profile, &self.claims);
        SignedProfile {
            key,
            kind: self.kind,
            profile: self.profile.clone(),
            claims: self.claims.clone(),
            sig: self.sign(&bytes),
        }
    }

    /// Load from a JSON file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let id: Identity = serde_json::from_str(&text)?;
        // Fail early on a corrupt file rather than at first sign.
        let _ = id.signing_key();
        Ok(id)
    }

    /// Save to a JSON file, readable only by this user.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        write_private(path, text.as_bytes())
    }
}

impl Signer for Identity {
    fn public(&self) -> PublicKey {
        Identity::public(self)
    }

    fn sign(&self, msg: &[u8]) -> String {
        let sig = self.signing_key().sign(msg);
        format!(
            "{ED25519_PREFIX}{}",
            data_encoding::HEXLOWER.encode(&sig.to_bytes())
        )
    }
}

/// Write a file with 0600 permissions on Unix. On Windows the user's
/// profile directory is already private to the user.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let id = Identity::generate("alice", Kind::Agent);
        let sig = id.sign(b"hello");
        assert!(sig.starts_with("ed25519:"));
        id.public().verify(b"hello", &sig).unwrap();
        assert!(id.public().verify(b"hellp", &sig).is_err());
        let other = Identity::generate("bob", Kind::Agent);
        assert!(other.public().verify(b"hello", &sig).is_err());
    }

    #[test]
    fn public_key_string_roundtrip() {
        let id = Identity::generate("alice", Kind::Human);
        let s = id.public().to_string();
        let back: PublicKey = s.parse().unwrap();
        assert_eq!(back, id.public());
        assert_eq!(id.public().fingerprint().len(), 16);
    }

    #[test]
    fn profile_is_signed() {
        let mut id = Identity::generate("alice", Kind::Agent);
        id.profile.vendor = Some("anthropic".into());
        let sp = id.signed_profile();
        sp.verify().unwrap();
        let mut tampered = sp.clone();
        tampered.profile.vendor = Some("someone-else".into());
        assert!(tampered.verify().is_err());
    }

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join(format!("diavlos-keys-{}", ulid::Ulid::generate()));
        let path = dir.join("default.json");
        let id = Identity::generate("alice", Kind::Agent);
        id.save(&path).unwrap();
        let back = Identity::load(&path).unwrap();
        assert_eq!(back.public(), id.public());
        assert_eq!(back.name, "alice");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
