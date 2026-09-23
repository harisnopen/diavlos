//! Outbound secret scan: refuse to send anything that looks like a common
//! API key, token or private key.
//!
//! This catches accidents: an agent pasting `.env` into a room, a key left
//! in an action's params. It is not a data-loss control. A regex cannot
//! stop an agent that means to get a secret out: base64, a split string or
//! a format not listed here goes straight past it.

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
                // The optional quote after the name is for JSON: `"password":"..."`.
                r#"(?i)\b(api[_-]?key|secret[_-]?key|access[_-]?token|auth[_-]?token|password)\b['"]?\s*[:=]\s*['"]?[A-Za-z0-9_\-/+=]{20,}"#,
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

/// Scan everything a sender writes into a message: the text, the data, the
/// action (verb, target and params) and the trace. Missing any one of them
/// is a way round the rest.
pub fn scan_message(
    text: &str,
    data: &serde_json::Value,
    action: Option<&crate::Action>,
    trace: Option<&str>,
) -> Option<&'static str> {
    if let Some(hit) = find_secret(text) {
        return Some(hit);
    }
    if !data.is_null() {
        if let Some(hit) = find_secret(&data.to_string()) {
            return Some(hit);
        }
    }
    if let Some(action) = action {
        let flat = serde_json::to_string(action).unwrap_or_default();
        if let Some(hit) = find_secret(&flat) {
            return Some(hit);
        }
    }
    trace.and_then(find_secret)
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
        assert_eq!(
            scan_message("fine", &data, None, None),
            Some("AWS access key")
        );
        assert_eq!(
            scan_message("fine", &serde_json::Value::Null, None, None),
            None
        );
    }

    fn action(verb: &str, target: &str, params: serde_json::Value) -> crate::Action {
        crate::Action {
            verb: verb.into(),
            target: target.into(),
            params,
        }
    }

    /// The review found params went unscanned: a key there was signed and
    /// sent. Every part of the action counts now.
    #[test]
    fn scans_every_part_of_an_action() {
        let null = serde_json::Value::Null;
        let key = "AKIAIOSFODNN7EXAMPLE";
        let in_params = action("deploy", "prod", serde_json::json!({"aws_key": key}));
        assert_eq!(
            scan_message("fine", &null, Some(&in_params), None),
            Some("AWS access key")
        );
        let in_target = action("deploy", key, serde_json::Value::Null);
        assert_eq!(
            scan_message("fine", &null, Some(&in_target), None),
            Some("AWS access key")
        );
        let clean = action(
            "deploy",
            "api-service",
            serde_json::json!({"version": "1.2"}),
        );
        assert_eq!(scan_message("fine", &null, Some(&clean), None), None);
    }

    #[test]
    fn scans_the_trace() {
        let null = serde_json::Value::Null;
        assert!(scan_message(
            "fine",
            &null,
            None,
            Some("ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD")
        )
        .is_some());
        assert_eq!(scan_message("fine", &null, None, Some("TICKET-42")), None);
    }

    /// Data and params are JSON, where a password sits as `"password":"..."`.
    #[test]
    fn catches_a_password_written_as_json() {
        let null = serde_json::Value::Null;
        let data = serde_json::json!({"password": "hunter2hunter2hunter2hunter2"});
        assert!(scan_message("fine", &data, None, None).is_some());
        let params = action(
            "login",
            "db",
            serde_json::json!({"api_key": "abcdefghijklmnopqrstuvwxyz012345"}),
        );
        assert!(scan_message("fine", &null, Some(&params), None).is_some());
        // A field that merely mentions the word is not a secret.
        let talk = serde_json::json!({"note": "reset the password tomorrow"});
        assert_eq!(scan_message("fine", &talk, None, None), None);
    }
}
