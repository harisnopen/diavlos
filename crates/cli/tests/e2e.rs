//! Two helpers on one machine trade messages, with public relays off so
//! the test needs no network beyond loopback. This is the "done when"
//! check for v0.1 week 1, run against the real binary.

use std::path::PathBuf;
use std::process::{Command, Output};

struct Home {
    dir: PathBuf,
    user: &'static str,
}

impl Home {
    fn new(tag: &str, user: &'static str) -> Home {
        let dir = std::env::temp_dir().join(format!("diavlos-e2e-{tag}-{}", ulid()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "[helper]\npublic_relays = false\nretry_secs = 1\n",
        )
        .unwrap();
        Home { dir, user }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_diavlos"))
            .env("DIAVLOS_HOME", &self.dir)
            .env("USER", self.user)
            .env("USERNAME", self.user)
            .args(args)
            .output()
            .expect("run diavlos")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "diavlos {args:?} failed with {:?}\nstdout: {}\nstderr: {}\nhelper.log:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            self.helper_log()
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// The tail of this home's helper log, for failure messages. The log
    /// lives in a temp dir that is removed on drop, so read it now.
    fn helper_log(&self) -> String {
        let text = std::fs::read_to_string(self.dir.join("helper.log")).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(40);
        lines[start..].join("\n")
    }

    fn fails_with(&self, args: &[&str], code: i32) -> String {
        let out = self.run(args);
        assert_eq!(
            out.status.code(),
            Some(code),
            "diavlos {args:?}\nstdout: {}\nstderr: {}\nhelper.log:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            self.helper_log()
        );
        String::from_utf8_lossy(&out.stderr).to_string()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.run(&["stop"]);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn ulid() -> String {
    format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

fn invite_token(note: &str) -> String {
    note.split_whitespace()
        .find(|w| w.starts_with("dv1."))
        .expect("invite token in note")
        .to_string()
}

#[test]
fn two_helpers_trade_messages_and_nothing_is_lost() {
    let a = Home::new("a", "haris");
    let b = Home::new("b", "bobhost");
    let c = Home::new("c", "mallory");

    // A makes a room and is its owner.
    let out = a.ok(&["new", "ops", "--about", "test"]);
    assert!(out.contains("made room ops"));

    // A invites bob; B joins with its own agent key.
    let note = a.ok(&["invite", "ops", "bob"]);
    let inv = invite_token(&note);
    let out = b.ok(&["--as", "fixer", "join", &inv]);
    assert!(out.contains("joined ops as bob"), "{out}");

    // Messages flow both ways, typed, signed, and `next` skips your own.
    b.ok(&[
        "--as",
        "fixer",
        "send",
        "ops",
        "found a bug",
        "--type",
        "task",
        "--trace",
        "t-1",
    ]);
    let got = a.ok(&["next", "ops", "--timeout", "20", "--json"]);
    let msg: serde_json::Value = serde_json::from_str(got.trim()).unwrap();
    assert_eq!(msg["from"], "bob");
    assert_eq!(msg["type"], "task");
    assert_eq!(msg["trace"], "t-1");
    assert_eq!(msg["seq"], 3);
    assert!(msg["sig"].as_str().unwrap().starts_with("ed25519:"));

    a.ok(&["send", "ops", "on it", "--type", "reply"]);
    let got = b.ok(&["--as", "fixer", "next", "ops", "--timeout", "20"]);
    assert!(got.contains("haris (reply): on it"), "{got}");

    // Nothing new: next times out with exit code 4.
    b.fails_with(&["--as", "fixer", "next", "ops", "--timeout", "1"], 4);

    // A stranger with the used invite is refused (6); a forged one is junk.
    c.fails_with(&["join", &inv], 6);
    c.fails_with(&["join", "dv1.notaninvite"], 1);

    // A goes offline. B's message waits on disk and is not lost.
    a.ok(&["stop"]);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let out = b.ok(&[
        "--as",
        "fixer",
        "send",
        "ops",
        "while you were out",
        "--type",
        "done",
    ]);
    assert!(out.starts_with("queued"), "{out}");
    let st = b.ok(&["status"]);
    assert!(st.contains("1 queued"), "{st}");
    let ob = b.ok(&["outbox"]);
    assert!(
        ob.contains("pending") && ob.contains("while you were out"),
        "{ob}"
    );
    // Only a failed or quarantined message can be retried or dropped.
    let queued_id = ob.split_whitespace().next().unwrap().to_string();
    b.fails_with(&["outbox", "drop", &queued_id], 1);

    // A comes back (any command starts the helper) and the message arrives.
    a.ok(&["status"]);
    let got = a.ok(&["next", "ops", "--timeout", "30"]);
    assert!(got.contains("bob (done): while you were out"), "{got}");
    // The home has it before the sender hears back; give the answer a
    // moment to arrive.
    let mut ob = String::new();
    for _ in 0..50 {
        ob = b.ok(&["outbox"]);
        if ob.contains("the outbox is empty") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(
        ob.contains("the outbox is empty"),
        "{ob}\n{}",
        b.helper_log()
    );

    // Reading never deletes: the whole log is still there.
    let all = a.ok(&["read", "ops", "--since", "1", "--limit", "50", "--json"]);
    let lines: Vec<&str> = all.lines().collect();
    assert_eq!(lines.len(), 5, "{all}");
    let prev_of_2: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert!(prev_of_2["prev"].as_str().unwrap().starts_with("sha256:"));

    // An observer may read but not talk (6). Someone not in the room gets 2.
    let note = a.ok(&["invite", "ops", "alice", "--role", "observer"]);
    let inv2 = invite_token(&note);
    c.ok(&["--as", "alice", "join", &inv2]);
    c.fails_with(&["--as", "alice", "send", "ops", "hi"], 6);
    c.fails_with(&["--as", "nobody", "send", "ops", "hi"], 2);
}

#[test]
fn reading_only_looks_unless_asked_to_ack() {
    let a = Home::new("bm", "haris");
    a.ok(&["new", "ops", "--about", "test"]);
    a.ok(&["send", "ops", "one"]);
    // A plain read is a pure view: read twice, see the same.
    let first = a.ok(&["read", "ops", "--json"]);
    assert!(first.contains("\"one\""), "{first}");
    let again = a.ok(&["read", "ops", "--json"]);
    assert_eq!(first, again);
    // --ack settles exactly what it printed, and the bookmark moves.
    let acked = a.ok(&["read", "ops", "--ack", "--json"]);
    assert!(acked.contains("\"one\""), "{acked}");
    a.ok(&["send", "ops", "two"]);

    // Reading from seq 1, as the web page does, is only a look back.
    let all = a.ok(&["read", "ops", "--since", "1", "--json"]);
    assert!(all.contains("\"one\"") && all.contains("\"two\""), "{all}");

    // So the next plain read gets "two", and only "two".
    let next = a.ok(&["read", "ops", "--json"]);
    assert!(next.contains("\"two\""), "{next}");
    assert!(!next.contains("\"one\""), "{next}");
    a.ok(&["stop"]);
}

#[test]
fn a_message_taken_and_not_acked_comes_back() {
    let a = Home::new("lease", "haris");
    a.ok(&["new", "ops", "--about", "test"]);
    let inv = invite_token(&a.ok(&["invite", "ops", "worker"]));
    a.ok(&["--as", "worker", "join", &inv]);
    a.ok(&["send", "ops", "fix the build", "--type", "task"]);

    // Taken with a short lease, and the worker "dies" without acking.
    let took = a.ok(&[
        "--as",
        "worker",
        "next",
        "ops",
        "--manual-ack",
        "--lease",
        "2",
        "--json",
    ]);
    let first: serde_json::Value = serde_json::from_str(took.trim()).unwrap();
    assert_eq!(first["message"]["text"], "fix the build");
    assert_eq!(first["delivery"]["attempt"], 1);
    let old_token = first["delivery"]["token"].as_str().unwrap().to_string();
    // While the lease holds, nothing else is owed.
    a.fails_with(&["--as", "worker", "next", "ops", "--timeout", "1"], 4);

    // The lease runs out: the same message comes round again.
    std::thread::sleep(std::time::Duration::from_secs(3));
    let again = a.ok(&[
        "--as",
        "worker",
        "next",
        "ops",
        "--manual-ack",
        "--json",
        "--timeout",
        "10",
    ]);
    let second: serde_json::Value = serde_json::from_str(again.trim()).unwrap();
    assert_eq!(second["message"]["id"], first["message"]["id"]);
    assert_eq!(second["delivery"]["attempt"], 2);

    // The first worker wakes up late: its token no longer settles anything.
    a.fails_with(&["--as", "worker", "ack", &old_token], 6);
    let token = second["delivery"]["token"].as_str().unwrap();
    // Nor can another key settle this one's delivery.
    a.fails_with(&["ack", token], 6);
    a.ok(&["--as", "worker", "ack", token]);
    a.fails_with(&["--as", "worker", "next", "ops", "--timeout", "1"], 4);
    let d = a.ok(&["--as", "worker", "deliveries", "ops"]);
    assert!(d.contains("nothing handed out"), "{d}");
    a.ok(&["stop"]);
}
