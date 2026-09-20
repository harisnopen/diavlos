//! The helper's config file: `~/.diavlos/config.toml`.
//!
//! Config file, not just flags. Zero telemetry by default; there is nothing
//! to opt into yet. The license slot is empty and does nothing.

use std::path::Path;

use diavlos_core::Limits;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub helper: HelperConfig,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub license: LicenseConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperConfig {
    /// Use the public relays run by n0 (iroh.computer) when a direct link is
    /// not possible. Set to false in a locked-down network and list your own
    /// relays in `relay_urls`. Relays only ever see encrypted bytes.
    #[serde(default = "default_true")]
    pub public_relays: bool,
    /// Your own relay servers (self-hosted iroh relay), HTTPS URLs on 443.
    #[serde(default)]
    pub relay_urls: Vec<String>,
    /// Telemetry. Off. Nothing is sent anywhere. Opt-in only, and there is
    /// nothing to opt into in v0.1.
    #[serde(default)]
    pub telemetry: bool,
    /// UDP port for peer links. Picked once at random and kept, so direct
    /// addresses stay stable across restarts. 0 means pick again.
    #[serde(default)]
    pub port: u16,
    /// Log level for the helper log: error, warn, info, debug, trace.
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Seconds between retries to reach a room's home when it is offline.
    #[serde(default = "default_retry_secs")]
    pub retry_secs: u64,
    /// Serve Prometheus metrics here, e.g. "127.0.0.1:9797". Empty: off.
    #[serde(default)]
    pub metrics_addr: String,
    /// Data classes this helper refuses to store or relay.
    #[serde(default)]
    pub refuse_classes: Vec<diavlos_core::DataClass>,
    /// Refuse to send anything that looks like an API key or private key.
    #[serde(default = "default_true")]
    pub secret_scan: bool,
    /// Encrypt message content at rest with a key in `inbox.key` (or the
    /// OS keychain).
    #[serde(default = "default_true")]
    pub encrypt_inbox: bool,
    /// Keep secret keys in the OS keychain when one is available (macOS
    /// Keychain, Windows Credential Manager, Linux Secret Service).
    #[serde(default = "default_true")]
    pub keychain: bool,
    /// How often retention runs, in seconds.
    #[serde(default = "default_retention_check_secs")]
    pub retention_check_secs: u64,
}

fn default_retention_check_secs() -> u64 {
    3600
}

fn default_true() -> bool {
    true
}
fn default_log_level() -> String {
    "info".into()
}
fn default_retry_secs() -> u64 {
    5
}

impl Default for HelperConfig {
    fn default() -> Self {
        HelperConfig {
            public_relays: true,
            relay_urls: Vec::new(),
            telemetry: false,
            port: 0,
            log_level: default_log_level(),
            retry_secs: default_retry_secs(),
            metrics_addr: String::new(),
            refuse_classes: Vec::new(),
            secret_scan: true,
            encrypt_inbox: true,
            keychain: true,
            retention_check_secs: default_retention_check_secs(),
        }
    }
}

/// Empty. Does nothing. Makes open-core a config change later, not a
/// refactor.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LicenseConfig {
    #[serde(default)]
    pub key: String,
}

const TEMPLATE: &str = r#"# Diavlos helper config. Every key is optional.

[helper]
# Use n0's public relays when a direct link is not possible. Relays only
# ever see encrypted bytes. Set to false in a locked-down network and list
# your own relays below.
public_relays = true
relay_urls = []
# Telemetry is off. Nothing is sent anywhere. There is nothing to opt into.
telemetry = false
# UDP port for peer links. Picked once at random and kept.
port = 0
log_level = "info"
retry_secs = 5
# Prometheus metrics on localhost, e.g. "127.0.0.1:9797". Empty is off.
metrics_addr = ""
# Data classes this helper refuses to store or relay: public, internal,
# confidential, pii.
refuse_classes = []
# Refuse to send anything that looks like an API key or a private key.
secret_scan = true
# Encrypt message content at rest.
encrypt_inbox = true
# Keep secret keys in the OS keychain when one is available.
keychain = true
# How often retention runs, in seconds.
retention_check_secs = 3600

[limits]
# Floods and loops: both limits are on by default.
per_minute_per_sender = 60
daily_per_room = 2000
burst_alert_percent = 80

[license]
# Empty. Does nothing.
key = ""
"#;

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(path)?;
        let cfg: Config =
            toml::from_str(&text).map_err(|e| anyhow::anyhow!("config {}: {e}", path.display()))?;
        Ok(cfg)
    }

    /// Write the commented template if no config exists yet.
    pub fn ensure(path: &Path) -> anyhow::Result<()> {
        if path.exists() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, TEMPLATE)?;
        Ok(())
    }

    /// Persist a chosen port so direct addresses stay stable.
    pub fn save_port(path: &Path, port: u16) -> anyhow::Result<()> {
        Config::ensure(path)?;
        let text = std::fs::read_to_string(path)?;
        let mut doc: toml::Table = text.parse().unwrap_or_default();
        let helper = doc
            .entry("helper")
            .or_insert_with(|| toml::Value::Table(Default::default()));
        if let toml::Value::Table(t) = helper {
            t.insert("port".into(), toml::Value::Integer(port as i64));
        }
        std::fs::write(path, toml::to_string(&doc)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_to_defaults() {
        let cfg: Config = toml::from_str(TEMPLATE).unwrap();
        assert!(cfg.helper.public_relays);
        assert!(!cfg.helper.telemetry);
        assert_eq!(cfg.limits.daily_per_room, 2000);
        assert_eq!(cfg.license.key, "");
    }
}
