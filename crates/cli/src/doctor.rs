//! `diavlos doctor`: checks config, network, keys, disk. Paste the output
//! in a support ticket. Never prints secrets.

use diavlos_client::proto::{Request, StatusResult};
use diavlos_client::{Client, Paths};
use serde_json::{json, Value};

use crate::config::Config;

pub async fn run(paths: &Paths) -> Value {
    let mut checks = Vec::new();
    let mut check = |name: &str, ok: bool, detail: String| {
        checks.push(json!({"check": name, "ok": ok, "detail": detail}));
    };

    check(
        "version",
        true,
        format!(
            "diavlos {} ({} {})",
            diavlos_core::VERSION,
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    );
    check(
        "home",
        paths.home.exists(),
        paths.home.display().to_string(),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(md) = std::fs::metadata(&paths.home) {
            let mode = md.permissions().mode() & 0o777;
            check(
                "home permissions",
                mode == 0o700,
                format!("{mode:o} (want 700)"),
            );
        }
    }
    match Config::load(&paths.config()) {
        Ok(c) => check(
            "config",
            true,
            format!(
                "{} (public_relays={}, relays={}, secret_scan={}, encrypt_inbox={}, metrics={})",
                paths.config().display(),
                c.helper.public_relays,
                c.helper.relay_urls.len(),
                c.helper.secret_scan,
                c.helper.encrypt_inbox,
                if c.helper.metrics_addr.is_empty() {
                    "off".to_string()
                } else {
                    c.helper.metrics_addr.clone()
                }
            ),
        ),
        Err(e) => check("config", false, format!("{e:#}")),
    }
    let keys: Vec<String> = std::fs::read_dir(paths.keys_dir())
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
                .map(|e| {
                    let label = e.path().file_stem().unwrap().to_string_lossy().to_string();
                    match std::fs::read_to_string(e.path())
                        .ok()
                        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    {
                        Some(v)
                            if v.get("secret_in_keychain").and_then(|b| b.as_bool())
                                == Some(true) =>
                        {
                            format!("{label} (secret in OS keychain)")
                        }
                        Some(_) => format!("{label} (file)"),
                        None => format!("{label} (unreadable)"),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    check(
        "keys",
        !keys.is_empty(),
        if keys.is_empty() {
            "none yet; made on first use".into()
        } else {
            keys.join(", ")
        },
    );
    check(
        "node key",
        paths.node_key().exists(),
        paths.node_key().display().to_string(),
    );
    let db_size = std::fs::metadata(paths.db()).map(|m| m.len()).unwrap_or(0);
    check(
        "inbox",
        paths.db().exists(),
        format!("{} ({} bytes)", paths.db().display(), db_size),
    );
    match fs4::available_space(&paths.home) {
        Ok(free) => check(
            "disk",
            free > 100 * 1024 * 1024,
            format!("{} MB free", free / 1024 / 1024),
        ),
        Err(e) => check("disk", false, format!("{e}")),
    }
    let proxy = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]
        .iter()
        .filter(|k| std::env::var_os(k).is_some())
        .map(|k| k.to_string())
        .collect::<Vec<_>>();
    check(
        "proxy",
        true,
        if proxy.is_empty() {
            "no proxy variables set".into()
        } else {
            format!("set: {}", proxy.join(", "))
        },
    );

    let client = Client::new(paths.clone());
    match client.call_if_running(&Request::Status).await {
        Ok(Some(v)) => match serde_json::from_value::<StatusResult>(v) {
            Ok(s) => {
                check("helper", true, format!("running, node {}", s.node));
                check(
                    "encrypted inbox",
                    s.encrypted_inbox,
                    if s.encrypted_inbox {
                        "on".into()
                    } else {
                        "off".into()
                    },
                );
                let relays = s
                    .network
                    .get("relays")
                    .and_then(|r| r.as_array())
                    .cloned()
                    .unwrap_or_default();
                let connected = relays
                    .iter()
                    .filter(|r| r["connected"].as_bool().unwrap_or(false))
                    .count();
                let public = Config::load(&paths.config())
                    .map(|c| c.helper.public_relays || !c.helper.relay_urls.is_empty())
                    .unwrap_or(true);
                check(
                    "relays",
                    !public || connected > 0,
                    if relays.is_empty() {
                        if public {
                            "configured, not connected yet".into()
                        } else {
                            "none configured (direct links only)".into()
                        }
                    } else {
                        format!("{connected} of {} connected", relays.len())
                    },
                );
                let addrs = s
                    .network
                    .get("bound")
                    .and_then(|b| b.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                check("sockets", addrs > 0, format!("{addrs} bound"));
                for r in &s.rooms {
                    check(
                        &format!("room {}", r.name),
                        r.connected && r.queued == 0,
                        format!(
                            "{}, {}, {} members, {} messages, {} queued{}",
                            if r.home { "home" } else { "member" },
                            if r.connected { "linked" } else { "offline" },
                            r.members,
                            r.messages,
                            r.queued,
                            if r.paused { ", paused" } else { "" }
                        ),
                    );
                }
            }
            Err(e) => check("helper", false, format!("bad status: {e}")),
        },
        Ok(None) => check(
            "helper",
            false,
            "not running (any command starts it)".into(),
        ),
        Err(e) => check("helper", false, format!("{e}")),
    }
    check("time", true, diavlos_core::message::now_ts());
    let all_ok = checks.iter().all(|c| c["ok"].as_bool().unwrap_or(false));
    json!({"ok": all_ok, "checks": checks})
}

pub fn print(report: &Value) {
    for c in report["checks"].as_array().cloned().unwrap_or_default() {
        println!(
            "{} {:<18} {}",
            if c["ok"].as_bool().unwrap_or(false) {
                "ok  "
            } else {
                "FAIL"
            },
            c["check"].as_str().unwrap_or(""),
            c["detail"].as_str().unwrap_or("")
        );
    }
}
