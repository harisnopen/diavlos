//! Outbound secret scan: refuse to send anything that looks like an API
//! key or a private key. A hijacked agent can't paste `.env` into the room.

use std::sync::OnceLock;

use regex::Regex;

struct Pattern {
    name: &'static str,
    re: Regex,
}

fn patterns() -> &'static [Pattern] {
    static P: OnceLock<Vec<Pattern>> = OnceLock::new();
    P.get_or_init(|| {
        let mk = |name: &'static str, re: &str| Pattern {
            name,
            re: Regex::new(re).expect("valid regex"),
        };
        vec![
            mk("private key", r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY( BLOCK)?-----"),
            mk("AWS access key", r"\b(AKIA|ASIA)[0-9A-Z]{16}\b"),
            mk("GitHub token", r"\b(gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})\b"),
            mk("OpenAI or Anthropic key", r"\bsk-(ant-)?[A-Za-z0-9_-]{20,}\b"),
            mk("Slack token", r"\bxox[abprs]-[A-Za-z0-9-]{10,}\b"),
            mk("Slack app token", r"\bxapp-[A-Za-z0-9-]{10,}\b"),
            mk("Google API key", r"\bAIza[0-9A-Za-z_-]{35}\b"),
            mk("Stripe key", r"\b[sr]k_(live|test)_[0-9a-zA-Z]{24,}\b"),
            mk("JWT", r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b"),
            mk("Diavlos key", r"ed25519:[0-9a-f]{64}\b(?:[^\S]|$)*\bsecret"),
            mk(
                "assignment that looks like a secret",
                r#"(?i)\b(api[_-]?key|secret[_-]?key|access[_-]?token|auth[_-]?token|password)\b\s*[:=]\s*['"]?[A-Za-z0-9_\-/+=]{20,}"#,
            ),
        ]
    })
}

/// Scan text. Returns what was found, if anything.
pub fn find_secret(text: &str) -> Option<&'static str> {
    patterns()
        .iter()
        .find(|p| p.re.is_match(text))
        .map(|p| p.name)
}

/// Scan a message's text and data together.
pub fn scan_message(text: &str, data: &serde_json::Value) -> Option<&'static str> {
    if let Some(hit) = find_secret(text) {
        return Some(hit);
    }
    if data.is_null() {
        return None;
    }
    find_secret(&data.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catches_common_keys() {
        assert_eq!(
            find_secret("here AKIAIOSFODNN7EXAMPLE ok"),
            Some("AWS access key")
        );
        assert_eq!(
            find_secret("token ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD"),
            Some("GitHub token")
        );
        assert_eq!(
            find_secret("use sk-ant-api03-abcdefghijklmnopqrstuvwxyz"),
            Some("OpenAI or Anthropic key")
        );
        assert_eq!(
            find_secret("-----BEGIN OPENSSH PRIVATE KEY-----\nabc"),
            Some("private key")
        );
        assert_eq!(
            find_secret("API_KEY=abcdefghijklmnopqrstuvwxyz1234"),
            Some("assignment that looks like a secret")
        );
        assert_eq!(
            find_secret("xoxb-1234567890-abcdefghij"),
            Some("Slack token")
        );
    }

    #[test]
    fn leaves_normal_text_alone() {
        for t in [
            "found a bug in auth",
            "deploy api-service v1.2 to prod?",
            "the password field is empty",
            "seq 42 prev sha256:9f3a",
            "please set the API key in the vault, not here",
        ] {
            assert_eq!(find_secret(t), None, "{t}");
        }
    }

    #[test]
    fn scans_data_too() {
        let data = serde_json::json!({"env": {"AWS": "AKIAIOSFODNN7EXAMPLE"}});
        assert_eq!(scan_message("fine", &data), Some("AWS access key"));
        assert_eq!(scan_message("fine", &serde_json::Value::Null), None);
    }
}
