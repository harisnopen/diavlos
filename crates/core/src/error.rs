//! One error type for the whole of Diavlos.
//!
//! The exit codes (CLI) and error codes (MCP) mean the same thing:
//! 2 = not in room, 3 = reached nobody, 4 = timed out, 5 = name already
//! taken, 6 = denied, 7 = room paused. Anything else is 1.

use thiserror::Error;

/// Exit code for "not in room".
pub const CODE_NOT_IN_ROOM: i32 = 2;
/// Exit code for "reached nobody".
pub const CODE_REACHED_NOBODY: i32 = 3;
/// Exit code for "timed out".
pub const CODE_TIMED_OUT: i32 = 4;
/// Exit code for "name already taken".
pub const CODE_NAME_TAKEN: i32 = 5;
/// Exit code for "denied".
pub const CODE_DENIED: i32 = 6;
/// Exit code for "room paused".
pub const CODE_ROOM_PAUSED: i32 = 7;

#[derive(Debug, Error)]
pub enum Error {
    #[error("not in room: {0}")]
    NotInRoom(String),
    #[error("reached nobody: {0}")]
    ReachedNobody(String),
    #[error("timed out")]
    TimedOut,
    #[error("name already taken: {0}")]
    NameTaken(String),
    #[error("denied: {0}")]
    Denied(String),
    #[error("room paused: {0}")]
    RoomPaused(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("bad signature: {0}")]
    BadSignature(String),
    #[error("over budget: {0}")]
    OverBudget(String),
    #[error("storage: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("migration: {0}")]
    Migration(#[from] rusqlite_migration::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// The exit code (CLI) or error code (MCP) for this error.
    pub fn code(&self) -> i32 {
        match self {
            Error::NotInRoom(_) => CODE_NOT_IN_ROOM,
            Error::ReachedNobody(_) => CODE_REACHED_NOBODY,
            Error::TimedOut => CODE_TIMED_OUT,
            Error::NameTaken(_) => CODE_NAME_TAKEN,
            Error::Denied(_) | Error::BadSignature(_) => CODE_DENIED,
            Error::RoomPaused(_) => CODE_ROOM_PAUSED,
            _ => 1,
        }
    }

    /// Rebuild an error from a code and a message that crossed a wire.
    /// Strips the label an earlier hop's `Display` put in front, so the
    /// text does not stack up as it travels.
    pub fn from_code(code: i32, msg: &str) -> Self {
        let msg = strip_labels(msg);
        let msg = msg.as_str();
        match code {
            CODE_NOT_IN_ROOM => Error::NotInRoom(msg.to_string()),
            CODE_REACHED_NOBODY => Error::ReachedNobody(msg.to_string()),
            CODE_TIMED_OUT => Error::TimedOut,
            CODE_NAME_TAKEN => Error::NameTaken(msg.to_string()),
            CODE_DENIED => Error::Denied(msg.to_string()),
            CODE_ROOM_PAUSED => Error::RoomPaused(msg.to_string()),
            _ => Error::Other(msg.to_string()),
        }
    }
}

const LABELS: &[&str] = &[
    "not in room: ",
    "reached nobody: ",
    "name already taken: ",
    "denied: ",
    "room paused: ",
    "invalid: ",
    "bad signature: ",
    "over budget: ",
    "storage: ",
    "io: ",
    "json: ",
];

fn strip_labels(msg: &str) -> String {
    let mut m = msg;
    loop {
        let mut stripped = false;
        for label in LABELS {
            if let Some(rest) = m.strip_prefix(label) {
                m = rest;
                stripped = true;
            }
        }
        if !stripped {
            return m.to_string();
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_roundtrip_without_stacking() {
        let e = Error::NameTaken("bob".into());
        let hop1 = Error::from_code(e.code(), &e.to_string());
        let hop2 = Error::from_code(hop1.code(), &hop1.to_string());
        assert_eq!(hop2.to_string(), "name already taken: bob");
        assert_eq!(hop2.code(), CODE_NAME_TAKEN);
        assert_eq!(Error::from_code(4, "timed out").code(), CODE_TIMED_OUT);
    }
}
