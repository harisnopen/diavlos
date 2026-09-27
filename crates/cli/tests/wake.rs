//! Wake rules against the real binary: a local HTTP server stands in for
//! the agent's platform, and a script for a local agent.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

const SECRET: &str = "wake-test-secret";

struct Home {
    dir: PathBuf,
    scale_ms: u64,
}

impl Home {
    fn new(tag: &str, scale_ms: u64) -> Home {
        let dir = std::env::temp_dir().join(format!("diavlos-wake-{tag}-{}", nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "[helper]\npublic_relays = false\nretry_secs = 1\nkeychain = false\n",
        )
        .unwrap();
        Home { dir, scale_ms }
    }

    fn run_as(&self, who: &str, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_diavlos"))
            .env("DIAVLOS_HOME", &self.dir)
            .env("USER", "haris")
            .env("USERNAME", "haris")
            .env("DIAVLOS_AS", who)
            .env("DIAVLOS_WAKE_SCALE_MS", self.scale_ms.to_string())
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .env("WAKE_SECRET", SECRET)
            .args(args)
            .output()
            .expect("run diavlos")
    }

    fn ok_as(&self, who: &str, args: &[&str]) -> String {
        let out = self.run_as(who, args);
        assert!(
            out.status.success(),
            "diavlos {args:?} as {who} failed\nstdout: {}\nstderr: {}\nhelper.log:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            std::fs::read_to_string(self.dir.join("helper.log")).unwrap_or_default()
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn ok(&self, args: &[&str]) -> String {
        self.ok_as("default", args)
    }

    /// A room `ops` owned by haris, with the agent `runner` in it on the
    /// same helper.
    fn room_with_runner(&self, class: Option<&str>) {
        let mut args = vec!["new", "ops"];
        if let Some(c) = class {
            args.extend(["--class", c]);
        }
        self.ok(&args);
        let note = self.ok(&["invite", "ops", "runner"]);
        let inv = note
            .split_whitespace()
            .find(|w| w.starts_with("dv1."))
            .unwrap()
            .to_string();
        self.ok_as("runner", &["join", &inv]);
    }

    fn events(&self) -> String {
        self.ok(&["events"])
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.run_as("default", &["stop"]);
        std::thread::sleep(Duration::from_millis(200));
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

#[derive(Clone, Debug)]
struct Hit {
    headers: HashMap<String, String>,
    body: String,
}

/// A one-thread HTTP server that records every request and answers with
/// `status`.
struct Receiver {
    url: String,
    hits: Arc<Mutex<Vec<Hit>>>,
}

impl Receiver {
    fn start(status: u16) -> Receiver {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://127.0.0.1:{}/wake",
            listener.local_addr().unwrap().port()
        );
        let hits = Arc::new(Mutex::new(Vec::new()));
        let status = Arc::new(AtomicU16::new(status));
        let (h, s) = (hits.clone(), status.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let mut reader = BufReader::new(conn.try_clone().unwrap());
                let mut headers = HashMap::new();
                let mut line = String::new();
                let _ = reader.read_line(&mut line); // request line
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.trim_end().split_once(':') {
                        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                let len: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0; len];
                let _ = reader.read_exact(&mut body);
                h.lock().unwrap().push(Hit {
                    headers,
                    body: String::from_utf8_lossy(&body).to_string(),
                });
                let code = s.load(Ordering::SeqCst);
                let _ = write!(
                    conn,
                    "HTTP/1.1 {code} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
            }
        });
        Receiver { url, hits }
    }

    fn hits(&self) -> Vec<Hit> {
        self.hits.lock().unwrap().clone()
    }

    fn wait_for(&self, n: usize, within: Duration) -> Vec<Hit> {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if self.hits().len() >= n {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.hits()
    }
}

fn verify(hit: &Hit) -> serde_json::Value {
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
    mac.update(hit.body.as_bytes());
    let want = format!(
        "sha256={}",
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    assert_eq!(
        hit.headers.get("diavlos-signature"),
        Some(&want),
        "bad signature"
    );
    let v: serde_json::Value = serde_json::from_str(&hit.body).unwrap();
    assert_eq!(
        hit.headers.get("diavlos-timestamp").map(String::as_str),
        v["ts"].as_str()
    );
    for banned in ["text", "data", "action"] {
        assert!(
            v.get(banned).is_none(),
            "{banned} leaked into a nudge: {}",
            hit.body
        );
    }
    v
}

fn add_url_rule(home: &Home, url: &str) -> String {
    let out = home.ok_as(
        "runner",
        &[
            "wake",
            "add",
            "ops",
            "--url",
            url,
            "--secret-env",
            "WAKE_SECRET",
        ],
    );
    out.split_whitespace()
        .nth(1)
        .unwrap()
        .trim_end_matches(':')
        .to_string()
}

#[test]
fn ten_messages_in_a_burst_are_one_signed_nudge_and_are_still_there() {
    let home = Home::new("burst", 1000);
    home.room_with_runner(None);
    let rx = Receiver::start(200);
    add_url_rule(&home, &rx.url);
    for i in 0..10 {
        home.ok(&["send", "ops", &format!("job {i}"), "--type", "task"]);
    }
    let hits = rx.wait_for(1, Duration::from_secs(15));
    // Give a second nudge time to show up if the burst were split.
    std::thread::sleep(Duration::from_secs(3));
    let hits_after = rx.hits();
    assert_eq!(
        hits_after.len(),
        1,
        "one nudge per burst, got {hits_after:?}"
    );
    let v = verify(&hits[0]);
    assert_eq!(v["count"], 10, "{v}");
    assert_eq!(v["room"], "ops");
    assert_eq!(v["to"], "runner");
    assert_eq!(v["event"], "wake");
    // A nudge settles nothing: the agent reads the real messages.
    let got = home.ok_as("runner", &["next", "ops", "--timeout", "5", "--json"]);
    assert!(got.contains("job 0"), "{got}");
    let events = home.events();
    assert!(
        events.contains("wake_fired") && events.contains("wake_ok"),
        "{events}"
    );
    assert!(
        !events.contains(SECRET) && !events.contains("/wake"),
        "{events}"
    );
}

#[test]
fn a_failing_url_retries_on_schedule_then_gives_up_and_loses_nothing() {
    // 1 "second" = 10 ms: retries at 0.3 s, 1.2 s, 6 s.
    let home = Home::new("fail", 10);
    home.room_with_runner(None);
    let rx = Receiver::start(500);
    add_url_rule(&home, &rx.url);
    home.ok(&["send", "ops", "please", "--type", "task"]);
    let hits = rx.wait_for(4, Duration::from_secs(20));
    assert_eq!(hits.len(), 4, "one try and three retries");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut events = home.events();
    while !events.contains("wake_gave_up") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        events = home.events();
    }
    assert_eq!(events.matches("wake_retry").count(), 3, "{events}");
    assert!(events.contains("wake_gave_up"), "{events}");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(rx.hits().len(), 4, "no nudge after giving up on the burst");
    let got = home.ok_as("runner", &["next", "ops", "--timeout", "5"]);
    assert!(got.contains("please"), "{got}");
}

#[test]
fn no_nudge_for_your_own_message_a_notice_or_a_paused_room() {
    let home = Home::new("quiet", 100);
    home.room_with_runner(None);
    let rx = Receiver::start(200);
    add_url_rule(&home, &rx.url);
    // The runner's own message, and a helper notice (someone joining).
    home.ok_as("runner", &["send", "ops", "my own"]);
    let note = home.ok(&["invite", "ops", "other"]);
    let inv = note
        .split_whitespace()
        .find(|w| w.starts_with("dv1."))
        .unwrap()
        .to_string();
    home.ok_as("other", &["join", &inv]);
    // A paused room fires nothing.
    home.ok(&["pause", "ops"]);
    std::thread::sleep(Duration::from_secs(3));
    assert!(rx.hits().is_empty(), "{:?}", rx.hits());
    // Resumed with a message waiting: now it wakes.
    home.ok(&["resume", "ops"]);
    home.ok(&["send", "ops", "go", "--type", "task"]);
    let hits = rx.wait_for(1, Duration::from_secs(10));
    assert_eq!(verify(&hits[0])["count"], 1);
}

#[test]
fn deliver_is_refused_in_a_confidential_room() {
    let home = Home::new("conf", 1000);
    home.room_with_runner(Some("confidential"));
    let out = home.run_as(
        "runner",
        &["wake", "add", "ops", "--exec", "/bin/true", "--deliver"],
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("confidential") && err.contains("--deliver"),
        "{err}"
    );
    // A nudge is fine there.
    home.ok_as("runner", &["wake", "add", "ops", "--exec", "/bin/true"]);
    // And a URL rule never delivers, in any room.
    let out = home.run_as(
        "runner",
        &[
            "wake",
            "add",
            "ops",
            "--url",
            "https://x.example/w",
            "--secret-env",
            "WAKE_SECRET",
            "--deliver",
        ],
    );
    assert!(!out.status.success());
}

#[cfg(unix)]
#[test]
fn an_exec_rule_survives_a_restart_and_can_be_tested_and_removed() {
    use std::os::unix::fs::PermissionsExt;
    let home = Home::new("exec", 100);
    home.room_with_runner(None);
    let out_file = home.dir.join("woken.txt");
    let script = home.dir.join("wake.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$DIAVLOS_COUNT $DIAVLOS_AS $DIAVLOS_ROOM ${{DIAVLOS_WAKE_TEST:-live}}\" >> {}\n",
            out_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let added = home.ok_as(
        "runner",
        &["wake", "add", "ops", "--exec", script.to_str().unwrap()],
    );
    let id = added
        .split_whitespace()
        .nth(1)
        .unwrap()
        .trim_end_matches(':')
        .to_string();

    // Test fires it once with a made-up nudge.
    home.ok(&["wake", "test", &id]);
    assert_eq!(
        std::fs::read_to_string(&out_file).unwrap().trim(),
        "1 runner ops 1"
    );

    // Stop the helper; the next command starts it again, rules and all.
    home.ok(&["stop"]);
    std::thread::sleep(Duration::from_millis(500));
    home.ok(&["send", "ops", "wake up", "--type", "task"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !std::fs::read_to_string(&out_file)
        .unwrap_or_default()
        .contains("live")
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(100));
    }
    let text = std::fs::read_to_string(&out_file).unwrap();
    assert!(text.lines().any(|l| l == "1 runner ops live"), "{text}");

    let list = home.ok(&["wake", "list"]);
    assert!(list.contains(&id) && list.contains("nudge"), "{list}");
    let doctor = home.ok(&["doctor"]);
    assert!(
        doctor.contains("wake rules") && doctor.contains(&id),
        "{doctor}"
    );
    home.ok(&["wake", "remove", &id]);
    assert!(!home.ok(&["wake", "list"]).contains(&id));
}

#[cfg(unix)]
#[test]
fn deliver_hands_over_the_message_and_acks_on_exit_zero() {
    use std::os::unix::fs::PermissionsExt;
    let home = Home::new("deliver", 100);
    home.room_with_runner(None);
    let out_file = home.dir.join("got.txt");
    let script = home.dir.join("handle.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$DIAVLOS_FROM:$DIAVLOS_TEXT\" >> {}\n",
            out_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    home.ok_as(
        "runner",
        &[
            "wake",
            "add",
            "ops",
            "--exec",
            script.to_str().unwrap(),
            "--deliver",
        ],
    );
    home.ok(&["send", "ops", "build it", "--type", "task"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::fs::read_to_string(&out_file)
        .unwrap_or_default()
        .is_empty()
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        std::fs::read_to_string(&out_file).unwrap().trim(),
        "haris:build it"
    );
    // Acked: nothing left for next.
    std::thread::sleep(Duration::from_millis(500));
    let out = home.run_as("runner", &["next", "ops", "--timeout", "1"]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}
