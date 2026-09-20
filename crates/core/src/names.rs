//! Names: ASCII lowercase only, so look-alikes can't exist.
//!
//! `alice` and `aIice` can never both be in a room, because `aIice` is not
//! a valid name at all. The same rule covers room names.

use crate::error::{Error, Result};

/// Longest allowed name.
pub const MAX_NAME_LEN: usize = 32;

/// Check a member or room name. Lowercase ASCII letters, digits, `-` and
/// `_`. Must start with a letter. 1 to 32 characters.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(Error::Invalid(format!(
            "name must be 1 to {MAX_NAME_LEN} characters: {name:?}"
        )));
    }
    let mut chars = name.chars();
    let first = chars.next().expect("non-empty");
    if !first.is_ascii_lowercase() {
        return Err(Error::Invalid(format!(
            "name must start with a lowercase ascii letter: {name:?}"
        )));
    }
    for c in name.chars() {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
            return Err(Error::Invalid(format!(
                "name may only use lowercase ascii letters, digits, '-' and '_': {name:?}"
            )));
        }
    }
    Ok(())
}

/// Turn any string into something that passes `validate_name`, or `None`
/// if nothing usable is left.
pub fn sanitize_name(raw: &str) -> Option<String> {
    let mut out = String::new();
    for c in raw.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' {
            out.push(c);
        } else if c == ' ' || c == '.' {
            out.push('-');
        }
        if out.len() >= MAX_NAME_LEN {
            break;
        }
    }
    // Collapse runs of '-' and trim them from both ends.
    let mut collapsed = String::with_capacity(out.len());
    for c in out.chars() {
        if c == '-' && collapsed.ends_with('-') {
            continue;
        }
        collapsed.push(c);
    }
    let mut out = collapsed.trim_matches('-').to_string();
    while let Some(first) = out.chars().next() {
        if first.is_ascii_lowercase() {
            break;
        }
        out.remove(0);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_names() {
        for n in ["alice", "bob-2", "fixer_agent", "a"] {
            assert!(validate_name(n).is_ok(), "{n}");
        }
    }

    #[test]
    fn rejects_lookalikes_and_junk() {
        for n in [
            "Alice",
            "aIice",
            "",
            "1abc",
            "al ice",
            "ali.ce",
            "αλίκη",
            "-x",
        ] {
            assert!(validate_name(n).is_err(), "{n}");
        }
        let long = "a".repeat(MAX_NAME_LEN + 1);
        assert!(validate_name(&long).is_err());
    }

    #[test]
    fn sanitizes() {
        assert_eq!(sanitize_name("Haris N."), Some("haris-n".into()));
        assert_eq!(sanitize_name("123"), None);
        assert_eq!(sanitize_name("J. Doe"), Some("j-doe".into()));
    }
}
