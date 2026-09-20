//! Policy hook. One rule in v0.1: which verbs need a human approve.
//! A small file per room. The rule engine grows later; the hook point is
//! what can't be added later without a rewrite.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::message::Message;

/// The rule file for one room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// Verbs (of an `action`) that must not run without a human approve.
    #[serde(default = "default_approve_verbs")]
    pub approve_verbs: Vec<String>,
}

fn default_approve_verbs() -> Vec<String> {
    ["delete", "deploy", "pay", "mail"]
        .map(String::from)
        .to_vec()
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            approve_verbs: default_approve_verbs(),
        }
    }
}

impl Policy {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Policy::default());
        }
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|e| crate::error::Error::Invalid(format!("policy: {e}")))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| crate::error::Error::Invalid(format!("policy: {e}")))?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Does this verb need a human-signed approve before it runs?
    pub fn requires_approve(&self, verb: &str) -> bool {
        self.approve_verbs.iter().any(|v| v == verb)
    }
}

/// The hook. The helper calls it for every message before it is stored.
/// v0.1 ships one implementation; the interface is what matters.
pub trait PolicyHook: Send + Sync {
    /// Return an error to refuse the message.
    fn check(&self, policy: &Policy, msg: &Message) -> Result<()>;
    /// True if the message's action needs an approve before it runs.
    fn needs_approve(&self, policy: &Policy, msg: &Message) -> bool {
        msg.action
            .as_ref()
            .map(|a| policy.requires_approve(&a.verb))
            .unwrap_or(false)
    }
}

/// The v0.1 hook: never refuses, only answers `needs_approve`.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultHook;

impl PolicyHook for DefaultHook {
    fn check(&self, _policy: &Policy, _msg: &Message) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_file_roundtrip() {
        let p = Policy::default();
        assert!(p.requires_approve("deploy"));
        assert!(!p.requires_approve("lint"));
        let dir = std::env::temp_dir().join(format!("diavlos-policy-{}", ulid::Ulid::generate()));
        let path = dir.join("policy.toml");
        assert_eq!(Policy::load(&path).unwrap(), Policy::default());
        let custom = Policy {
            approve_verbs: vec!["rm".into()],
        };
        custom.save(&path).unwrap();
        assert_eq!(Policy::load(&path).unwrap(), custom);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
