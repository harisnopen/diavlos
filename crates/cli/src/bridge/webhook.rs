//! Wake nudges over HTTPS: the one thing a wake rule sends off the machine.
//!
//! A nudge says *that* messages wait for someone, never *what* they say:
//! the body is built here from counts and ids only, so there is no field
//! for text, data or an action to leak through. It is signed with the
//! rule's secret so the receiver can tell it came from this helper and is
//! fresh. Lives next to the other bridges and uses the same HTTP client.

use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use sha2::Sha256;

/// How long one POST may take.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Everything a nudge carries. Adding a field here is adding something
/// that leaves the machine: it must never be message content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Nudge {
    pub v: u32,
    pub event: &'static str,
    pub room: String,
    pub to: String,
    pub count: u64,
    pub newest_seq: u64,
    pub newest_id: String,
    pub ts: String,
}

impl Nudge {
    pub fn new(
        room: &str,
        to: &str,
        count: u64,
        newest_seq: u64,
        newest_id: &str,
        ts: &str,
    ) -> Self {
        Nudge {
            v: 1,
            event: "wake",
            room: room.to_string(),
            to: to.to_string(),
            count,
            newest_seq,
            newest_id: newest_id.to_string(),
            ts: ts.to_string(),
        }
    }

    pub fn body(&self) -> String {
        serde_json::to_string(self).expect("a nudge always serializes")
    }
}

/// The client for nudges. It never follows a redirect: a nudge goes to
/// the address the user gave or nowhere, and a 3xx counts as a failure.
/// Proxy settings come from the environment.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// `sha256=<hex>` of HMAC-SHA256 over the raw body.
pub fn sign(secret: &[u8], body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC takes any key length");
    mac.update(body.as_bytes());
    format!(
        "sha256={}",
        data_encoding::HEXLOWER.encode(&mac.finalize().into_bytes())
    )
}

/// HTTPS only; plain http only to this machine.
pub fn check_url(url: &str) -> Result<(), String> {
    let u = reqwest::Url::parse(url).map_err(|e| format!("{url:?} is not a URL: {e}"))?;
    match u.scheme() {
        "https" => {}
        "http"
            if matches!(
                u.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
            ) => {}
        "http" => return Err("plain http is allowed for localhost only; use https".into()),
        other => return Err(format!("{other}: is not allowed; use https")),
    }
    if u.host_str().is_none() {
        return Err(format!("{url:?} has no host"));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("put the secret in --secret-env, not in the URL".into());
    }
    Ok(())
}

/// The host of a URL, for logs: never the path or query, which may carry
/// a token.
pub fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_else(|| "?".into())
}

/// POST one nudge. Ok on any 2xx; Err with a short reason otherwise.
pub async fn post(
    http: &reqwest::Client,
    url: &str,
    secret: &[u8],
    nudge: &Nudge,
) -> Result<(), String> {
    let body = nudge.body();
    let res = http
        .post(url)
        .timeout(TIMEOUT)
        .header("Content-Type", "application/json")
        .header("User-Agent", concat!("diavlos/", env!("CARGO_PKG_VERSION")))
        .header("Diavlos-Timestamp", &nudge.ts)
        .header("Diavlos-Signature", sign(secret, &body))
        .body(body)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "timed out".to_string()
            } else if e.is_connect() {
                "could not connect".to_string()
            } else {
                "request failed".to_string()
            }
        })?;
    if res.status().is_success() {
        Ok(())
    } else {
        Err(format!("HTTP {}", res.status().as_u16()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nudge_carries_no_content() {
        let n = Nudge::new("ops", "runner", 3, 412, "m_01J", "2026-09-27T14:02:11Z");
        let v: serde_json::Value = serde_json::from_str(&n.body()).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        for banned in ["text", "data", "action", "sig", "from"] {
            assert!(!keys.contains(&banned), "{banned} must never be in a nudge");
        }
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(
            sorted,
            [
                "count",
                "event",
                "newest_id",
                "newest_seq",
                "room",
                "to",
                "ts",
                "v"
            ]
        );
        assert_eq!(v["event"], "wake");
        assert_eq!(v["count"], 3);
    }

    /// HMAC-SHA256 by the book (RFC 2104), from SHA-256 alone, to check
    /// `sign` against something that does not share its code.
    fn hmac_by_hand(key: &[u8], msg: &[u8]) -> String {
        use sha2::Digest;
        let mut k = [0u8; 64];
        if key.len() > 64 {
            k[..32].copy_from_slice(&Sha256::digest(key));
        } else {
            k[..key.len()].copy_from_slice(key);
        }
        let pad = |b: u8| k.iter().map(|x| x ^ b).collect::<Vec<u8>>();
        let inner = Sha256::digest([pad(0x36), msg.to_vec()].concat());
        let outer = Sha256::digest([pad(0x5c), inner.to_vec()].concat());
        format!("sha256={}", data_encoding::HEXLOWER.encode(&outer))
    }

    #[test]
    fn signature_is_hmac_sha256_over_the_body() {
        // A fresh random key each run, short and longer than a block.
        for len in [16usize, 100] {
            let key: Vec<u8> = (0..len).map(|_| rand::random::<u8>()).collect();
            let body = r#"{"v":1,"event":"wake","count":3}"#;
            assert_eq!(sign(&key, body), hmac_by_hand(&key, body.as_bytes()));
        }
    }

    #[test]
    fn https_only_except_localhost() {
        assert!(check_url("https://hooks.example.com/runner").is_ok());
        assert!(check_url("http://localhost:8080/wake").is_ok());
        assert!(check_url("http://127.0.0.1:9/x").is_ok());
        assert!(check_url("http://hooks.example.com/runner").is_err());
        assert!(check_url("ftp://example.com").is_err());
        assert!(check_url("https://user:pw@example.com").is_err());
        assert!(check_url("not a url").is_err());
    }

    #[test]
    fn logs_get_the_host_only() {
        assert_eq!(
            host_of("https://hooks.example.com/runner?token=abc"),
            "hooks.example.com"
        );
    }
}
