//! Policy hook. One rule in v0.1: which verbs need a human approve.
//! A small file per room. The rule engine grows later; the hook point is
//! what can't be added later without a rewrite.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::files::{HARD_MAX_FILES, HARD_MAX_FILE_BYTES};
use crate::message::{DataClass, Message};

const MB: u64 = 1024 * 1024;

/// The rule file for one room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// Verbs (of an `action`) that must not run without a human approve.
    #[serde(default = "default_approve_verbs")]
    pub approve_verbs: Vec<String>,
    /// How deep one task may hand work on: a task sent in reply to a task
    /// sent in reply to a task, and so on. A task that would go deeper, or
    /// hand work back to an agent already in that chain, is refused. 0 turns
    /// the check off.
    #[serde(default = "default_max_task_hops")]
    pub max_task_hops: u32,
    /// Files on messages: "any", "safe" or "off". Unset means "any", or
    /// "off" in a room whose class is confidential or pii.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<FileMode>,
    /// Biggest single file, in MB. Never over 1024.
    #[serde(default = "default_max_file_mb")]
    pub max_file_mb: u64,
    #[serde(default = "default_max_files_per_message")]
    pub max_files_per_message: usize,
    /// What one member may upload to the home in 24 hours, in MB.
    #[serde(default = "default_daily_file_mb")]
    pub daily_file_mb_per_member: u64,
    /// Everything the home keeps for this room, in MB.
    #[serde(default = "default_room_file_store_mb")]
    pub room_file_store_mb: u64,
    /// Days a file is kept after everyone it was for has fetched it.
    #[serde(default = "default_file_keep_days")]
    pub file_keep_days: u64,
    /// Days a file is kept in any case.
    #[serde(default = "default_file_max_days")]
    pub file_max_days: u64,
}

/// Which files a room takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileMode {
    /// Everything, with warnings for the reader.
    Any,
    /// Plain text, common pictures, PDF and ZIP, checked by their bytes.
    Safe,
    /// No files.
    Off,
}

impl FileMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileMode::Any => "any",
            FileMode::Safe => "safe",
            FileMode::Off => "off",
        }
    }
}

fn default_max_file_mb() -> u64 {
    25
}
fn default_max_files_per_message() -> usize {
    5
}
fn default_daily_file_mb() -> u64 {
    200
}
fn default_room_file_store_mb() -> u64 {
    2048
}
fn default_file_keep_days() -> u64 {
    7
}
fn default_file_max_days() -> u64 {
    30
}

fn default_max_task_hops() -> u32 {
    4
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
            max_task_hops: default_max_task_hops(),
            files: None,
            max_file_mb: default_max_file_mb(),
            max_files_per_message: default_max_files_per_message(),
            daily_file_mb_per_member: default_daily_file_mb(),
            room_file_store_mb: default_room_file_store_mb(),
            file_keep_days: default_file_keep_days(),
            file_max_days: default_file_max_days(),
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

    /// Which files the room takes, given its data class.
    pub fn file_mode(&self, class: DataClass) -> FileMode {
        self.files.unwrap_or(match class {
            DataClass::Confidential | DataClass::Pii => FileMode::Off,
            DataClass::Public | DataClass::Internal => FileMode::Any,
        })
    }

    /// Biggest single file, in bytes, never over the hard cap.
    pub fn max_file_bytes(&self) -> u64 {
        (self.max_file_mb.saturating_mul(MB)).min(HARD_MAX_FILE_BYTES)
    }

    pub fn max_files(&self) -> usize {
        self.max_files_per_message.min(HARD_MAX_FILES)
    }

    pub fn daily_file_bytes(&self) -> u64 {
        self.daily_file_mb_per_member.saturating_mul(MB)
    }

    pub fn room_file_bytes(&self) -> u64 {
        self.room_file_store_mb.saturating_mul(MB)
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
            max_task_hops: 2,
            files: Some(FileMode::Safe),
            max_file_mb: 5000,
            ..Policy::default()
        };
        custom.save(&path).unwrap();
        assert_eq!(Policy::load(&path).unwrap(), custom);
        std::fs::remove_dir_all(dir).unwrap();
        // The hard cap wins over the file.
        assert_eq!(custom.max_file_bytes(), HARD_MAX_FILE_BYTES);
    }

    #[test]
    fn files_default_off_only_in_sensitive_rooms() {
        let p = Policy::default();
        assert_eq!(p.file_mode(DataClass::Internal), FileMode::Any);
        assert_eq!(p.file_mode(DataClass::Public), FileMode::Any);
        assert_eq!(p.file_mode(DataClass::Pii), FileMode::Off);
        assert_eq!(p.file_mode(DataClass::Confidential), FileMode::Off);
        let on: Policy = toml::from_str("files = \"safe\"\n").unwrap();
        assert_eq!(on.file_mode(DataClass::Pii), FileMode::Safe);
        assert_eq!(on.max_file_bytes(), 25 * MB);
    }
}
