//! Where secrets live: the OS keychain when there is one, else a 0600 file.
//!
//! The identity file always exists (name, kind, profile). Its `secret`
//! field is empty when the secret is in the keychain.

use std::path::Path;

use anyhow::Context as _;
use diavlos_client::Paths;
use diavlos_core::{Error, Identity, Result};
use serde::{Deserialize, Serialize};

const SERVICE: &str = "diavlos";

fn keychain_available() -> bool {
    keyring::Entry::store_status().is_ok()
}

fn entry(paths: &Paths, what: &str) -> Option<keyring::Entry> {
    let tag = {
        let h = diavlos_core::canonical::sha256_bytes(paths.home.to_string_lossy().as_bytes());
        h.trim_start_matches("sha256:")[..12].to_string()
    };
    keyring::Entry::new(SERVICE, &format!("{tag}/{what}")).ok()
}

/// The on-disk shape when the secret is in the keychain.
#[derive(Serialize, Deserialize)]
struct Stub {
    name: String,
    kind: diavlos_core::Kind,
    #[serde(default)]
    profile: diavlos_core::Profile,
    #[serde(default)]
    claims: serde_json::Value,
    #[serde(default)]
    created: String,
    /// Marker: the secret is in the OS keychain.
    secret_in_keychain: bool,
}

/// Every key file here and its kind, read from the file alone: no secret is
/// loaded, from the file or the keychain. Unreadable files are skipped.
pub fn key_kinds(paths: &Paths) -> Vec<(String, diavlos_core::Kind)> {
    let Ok(dir) = std::fs::read_dir(paths.keys_dir()) else {
        return Vec::new();
    };
    let mut out: Vec<(String, diavlos_core::Kind)> = dir
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let label = e.path().file_stem()?.to_string_lossy().to_string();
            let text = std::fs::read_to_string(e.path()).ok()?;
            let v: serde_json::Value = serde_json::from_str(&text).ok()?;
            let kind = serde_json::from_value(v.get("kind")?.clone()).ok()?;
            Some((label, kind))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

pub fn save_identity(paths: &Paths, label: &str, id: &Identity, keychain: bool) -> Result<()> {
    let path = paths.key(label);
    if keychain && keychain_available() {
        if let Some(e) = entry(paths, &format!("key/{label}")) {
            if e.set_password(&id.secret_hex()).is_ok() {
                let stub = Stub {
                    name: id.name.clone(),
                    kind: id.kind,
                    profile: id.profile.clone(),
                    claims: id.claims.clone(),
                    created: id.created.clone(),
                    secret_in_keychain: true,
                };
                let text = serde_json::to_string_pretty(&stub)?;
                diavlos_core::keys::write_private(&path, text.as_bytes())?;
                return Ok(());
            }
        }
    }
    id.save(&path)
}

pub fn load_identity(paths: &Paths, label: &str, keychain: bool) -> Result<Identity> {
    let path = paths.key(label);
    let text = std::fs::read_to_string(&path)?;
    let v: serde_json::Value = serde_json::from_str(&text)?;
    if v.get("secret_in_keychain").and_then(|b| b.as_bool()) == Some(true) {
        let stub: Stub = serde_json::from_value(v)?;
        if !keychain {
            return Err(Error::Other(format!(
                "key {label} is in the OS keychain but keychain = false in config"
            )));
        }
        let e = entry(paths, &format!("key/{label}"))
            .ok_or_else(|| Error::Other("keychain not available".into()))?;
        let secret = e
            .get_password()
            .map_err(|err| Error::Other(format!("keychain: {err}")))?;
        return Identity::from_parts(
            &stub.name,
            &secret,
            stub.kind,
            stub.profile,
            stub.claims,
            &stub.created,
        );
    }
    Identity::load(&path)
}

/// The key that encrypts message content at rest.
pub fn inbox_key(paths: &Paths, keychain: bool) -> anyhow::Result<[u8; 32]> {
    let file = paths.home.join("inbox.key");
    if keychain && keychain_available() {
        if let Some(e) = entry(paths, "inbox") {
            match e.get_password() {
                Ok(hex) => return decode_key(&hex),
                Err(keyring::Error::NoEntry) => {
                    if !file.exists() {
                        let key: [u8; 32] = rand::random();
                        let hex = data_encoding::HEXLOWER.encode(&key);
                        if e.set_password(&hex).is_ok() {
                            return Ok(key);
                        }
                    }
                }
                Err(_) => {}
            }
        }
    }
    read_or_make_key_file(&file)
}

fn read_or_make_key_file(file: &Path) -> anyhow::Result<[u8; 32]> {
    if file.exists() {
        let hex = std::fs::read_to_string(file)?;
        return decode_key(hex.trim());
    }
    let key: [u8; 32] = rand::random();
    let hex = data_encoding::HEXLOWER.encode(&key);
    diavlos_core::keys::write_private(file, hex.as_bytes()).context("write inbox key")?;
    Ok(key)
}

fn decode_key(hex: &str) -> anyhow::Result<[u8; 32]> {
    let raw = data_encoding::HEXLOWER
        .decode(hex.trim().as_bytes())
        .context("inbox key is not hex")?;
    raw.try_into()
        .map_err(|_| anyhow::anyhow!("inbox key has the wrong length"))
}
