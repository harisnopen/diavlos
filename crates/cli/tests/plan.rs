//! The plan's "done when" checks, run against the real binary with public
//! relays off. Each test gets its own helpers in temp dirs.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

struct Home {
    dir: PathBuf,
    user: &'static str,
    identity: &'static str,
    env: Vec<(String, String)>,
}

impl Home {
    fn new(tag: &str, user: &'static str, identity: &'static str) -> Home {
        Home::with_config(
            tag,
            user,
            identity,
            "[helper]\npublic_relays = false\nretry_secs = 1\n",
        )
    }

    fn with_config(tag: &str, user: &'static str, identity: &'static str, config: &str) -> Home {
        let dir = std::env::temp_dir().join(format!("diavlos-plan-{tag}-{}", nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), config).unwrap();
        Home {
            dir,
            user,
            identity,
            env: Vec::new(),
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_diavlos"));
        c.env("DIAVLOS_HOME", &self.dir)
            .env("USER", self.user)
            .env("USERNAME", self.user)
            .env("DIAVLOS_AS", self.identity)
            .args(args);
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().expect("run diavlos")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "diavlos {args:?} failed with {:?}\nstdout: {}\nstderr: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn code(&self, args: &[&str]) -> i32 {
        self.run(args).status.code().unwrap_or(-1)
    }

    fn fails_with(&self, args: &[&str], code: i32) -> String {
        let out = self.run(args);
        assert_eq!(
            out.status.code(),
            Some(code),
            "diavlos {args:?}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stderr).to_string()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let out = self.ok(args);
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("not json ({e}): {out}"))
    }

    fn invite(&self, room: &str, name: &str, extra: &[&str]) -> String {
        let mut args = vec!["invite", room, name];
        args.extend_from_slice(extra);
        let note = self.ok(&args);
        note.split_whitespace()
            .find(|w| w.starts_with("dv1."))
            .expect("invite token")
            .to_string()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.run(&["stop"]);
        std::thread::sleep(Duration::from_millis(300));
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn nanos() -> String {
    format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

fn settle() {
    std::thread::sleep(Duration::from_millis(700));
}

#[test]
fn claims_approvals_and_owner_controls() {
    let a = Home::new("a", "haris", "default");
    let b = Home::new("b", "bobhost", "fixer");
    let c = Home::new("c", "alicehost", "alice");
    let d = Home::new("d", "carolhost", "carol");

    a.ok(&["new", "ops"]);
    let inv = a.invite("ops", "bob", &[]);
    b.ok(&["join", &inv]);
    let inv = a.invite("ops", "alice", &["--human"]);
    c.ok(&["join", &inv]);

    // who: kind, role, fingerprint.
    let who = a.json(&["who", "ops", "--json"]);
    let alice = who
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "alice")
        .unwrap();
    assert_eq!(alice["kind"], "human");
    assert_eq!(alice["role"], "approver");
    assert_eq!(alice["fingerprint"].as_str().unwrap().len(), 16);

    // Claims: first wins, second is told no.
    let task = b.json(&["send", "ops", "fix auth", "--type", "task", "--json"]);
    let tid = task["id"].as_str().unwrap();
    a.ok(&["claim", "ops", tid]);
    b.fails_with(&["claim", "ops", tid], 6);
    a.ok(&["release", "ops", tid]);
    b.ok(&["claim", "ops", tid]);
    a.fails_with(&["release", "ops", tid], 6);
    b.ok(&["release", "ops", tid]);

    // Ask a human first: the approve signs the exact action, works once,
    // and only a human key counts.
    c.ok(&["read", "ops", "--limit", "500"]);
    let action =
        r#"{"verb":"deploy","target":"api-service","params":{"version":"1.2","env":"prod"}}"#;
    let asking = b
        .cmd(&[
            "ask",
            "ops",
            "Deploy?",
            "--action",
            action,
            "--timeout",
            "30",
            "--json",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(2));
    let q = c.json(&["next", "ops", "--timeout", "10", "--json"]);
    assert_eq!(q["type"], "question");
    let qid = q["id"].as_str().unwrap();
    b.fails_with(
        &["send", "ops", "yes", "--type", "approve", "--reply-to", qid],
        6,
    );
    // Talk is not permission: a plain reply does not end an ask that
    // carries an action.
    c.ok(&[
        "send",
        "ops",
        "let me look",
        "--type",
        "reply",
        "--reply-to",
        qid,
    ]);
    std::thread::sleep(Duration::from_secs(1));
    c.ok(&["send", "ops", "--type", "approve", "--reply-to", qid]);
    let out = asking.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let reply: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply["type"], "approve");
    assert_eq!(reply["once"], true);
    assert!(reply["action_hash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    a.ok(&["check-approve", "ops", action]);
    a.fails_with(&["check-approve", "ops", action], 6);
    a.fails_with(
        &[
            "check-approve",
            "ops",
            r#"{"verb":"deploy","target":"api-service","params":{"version":"1.3","env":"prod"}}"#,
        ],
        6,
    );

    // Deny: logged like a yes; the asker gets exit 6.
    let asking = b
        .cmd(&[
            "ask",
            "ops",
            "Delete the db?",
            "--action",
            r#"{"verb":"delete","target":"db","params":{}}"#,
            "--timeout",
            "30",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(2));
    let q = c.json(&["next", "ops", "--timeout", "10", "--json"]);
    c.ok(&[
        "deny",
        "ops",
        q["id"].as_str().unwrap(),
        "--reason",
        "not now",
    ]);
    let out = asking.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(6));

    // Roles: a chat member can't give orders; grants can expire.
    a.ok(&["grant", "ops", "bob", "--role", "chat"]);
    b.fails_with(&["send", "ops", "do x", "--type", "task"], 6);
    a.ok(&[
        "grant",
        "ops",
        "bob",
        "--role",
        "task-giver",
        "--until",
        "2020-01-01",
    ]);
    b.fails_with(&["send", "ops", "hi"], 6);
    a.ok(&["grant", "ops", "bob", "--role", "task-giver"]);
    b.ok(&["send", "ops", "do x", "--type", "task"]);

    // Kill switches: mute, pause, revoke.
    a.ok(&["mute", "ops", "bob"]);
    b.fails_with(&["send", "ops", "hi"], 6);
    a.ok(&["mute", "ops", "bob", "--off"]);
    b.ok(&["send", "ops", "hi again"]);
    a.ok(&["pause", "ops"]);
    b.fails_with(&["send", "ops", "hi"], 7);
    a.fails_with(&["send", "ops", "hi"], 7);
    a.ok(&["resume", "ops"]);
    b.ok(&["send", "ops", "back"]);
    let inv = a.invite("ops", "carol", &[]);
    d.ok(&["join", &inv]);
    d.ok(&["send", "ops", "hello"]);
    a.ok(&["revoke", "ops", "carol"]);
    settle();
    d.fails_with(&["send", "ops", "still here?"], 6);
    let who = a.json(&["who", "ops", "--json"]);
    let carol = who
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "carol")
        .unwrap();
    assert_eq!(carol["revoked"], true);

    // Two agents on one laptop: a second key joins a room that lives on
    // this very helper, no network needed.
    let inv = a.invite("ops", "scanner", &[]);
    let mut a2 = Home {
        dir: a.dir.clone(),
        user: "haris",
        identity: "scanner",
        env: Vec::new(),
    };
    a2.ok(&["join", &inv]);
    a.ok(&["read", "ops", "--limit", "500"]);
    a2.ok(&["send", "ops", "same laptop", "--type", "task"]);
    let got = a.ok(&["next", "ops", "--timeout", "10"]);
    assert!(got.contains("scanner (task): same laptop"), "{got}");
    a2.dir = std::path::PathBuf::from("/nonexistent-so-drop-does-nothing");

    // Secrets never leave the machine.
    b.fails_with(&["send", "ops", "key AKIAIOSFODNN7EXAMPLE"], 6);
    b.fails_with(
        &["send", "ops", "key: -----BEGIN OPENSSH PRIVATE KEY----- x"],
        6,
    );
}

#[test]
fn export_verify_events_watch_hold_rotate() {
    let a = Home::new("a2", "haris", "default");
    let b = Home::new("b2", "bobhost", "fixer");
    a.ok(&["new", "ops", "--retention", "30"]);
    let inv = a.invite("ops", "bob", &[]);
    b.ok(&["join", &inv]);
    b.ok(&[
        "send",
        "ops",
        "found a bug",
        "--type",
        "task",
        "--trace",
        "t-1",
    ]);
    a.ok(&["send", "ops", "on it", "--type", "reply"]);

    // Export a signed bundle; verify it with no helper involved.
    let bundle = a.ok(&["export", "ops"]);
    let path = a.dir.join("bundle.jsonl");
    std::fs::write(&path, &bundle).unwrap();
    let out = a.ok(&["verify", path.to_str().unwrap()]);
    assert!(out.contains("OK:"), "{out}");
    let bad = bundle.replace("found a bug", "rm -rf");
    let bad_path = a.dir.join("bad.jsonl");
    std::fs::write(&bad_path, bad).unwrap();
    assert_eq!(a.code(&["verify", bad_path.to_str().unwrap()]), 1);
    // A member's export verifies too.
    let bundle = b.ok(&["export", "ops", "--since", "2026-01-01"]);
    let path = b.dir.join("b.jsonl");
    std::fs::write(&path, &bundle).unwrap();
    assert!(b.ok(&["verify", path.to_str().unwrap()]).contains("OK:"));

    // Events: JSONL of what the helper did, no content.
    let events = a.ok(&["events"]);
    assert!(events.lines().count() >= 3, "{events}");
    assert!(events.contains("\"kind\":\"room_made\""));
    assert!(events.contains("\"kind\":\"member_joined\""));
    assert!(!events.contains("found a bug"));

    // watch --exec: the message goes in env vars, never on the command line.
    let script = b.dir.join("on-msg.sh");
    let outfile = b.dir.join("watch.out");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            &script,
            "#!/bin/sh\necho \"from=$DIAVLOS_FROM type=$DIAVLOS_TYPE key=$DIAVLOS_FROM_KEY\" >> \"$WATCH_OUT\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut watching = b
            .cmd(&["watch", "ops", "--exec", script.to_str().unwrap()])
            .env("WATCH_OUT", &outfile)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_secs(1));
        a.ok(&["send", "ops", "smoke tests passed", "--type", "done"]);
        std::thread::sleep(Duration::from_secs(2));
        let _ = watching.kill();
        let _ = watching.wait();
        let got = std::fs::read_to_string(&outfile).unwrap_or_default();
        assert!(got.contains("from=haris type=done key=ed25519:"), "{got}");
    }

    // Legal hold shows up as a signed control message.
    a.ok(&["hold", "ops", "--on"]);
    let log = a.ok(&["read", "ops", "--since", "1", "--limit", "200"]);
    assert!(log.contains("legal hold on"), "{log}");

    // Rotate: everyone out, old room kept for audit, re-invite who you keep.
    let out = a.ok(&["rotate", "ops"]);
    assert!(out.contains("rotated"), "{out}");
    settle();
    assert_ne!(b.code(&["send", "ops", "after rotate"]), 0);
    let st = b.ok(&["status"]);
    assert!(st.contains("rotated"), "{st}");
    let inv = a.invite("ops", "bob", &[]);
    b.ok(&["join", &inv]);
    b.ok(&["send", "ops", "back in"]);
    let st = a.ok(&["status"]);
    assert!(st.contains("ops-rotated-"), "{st}");

    // doctor passes on a healthy helper.
    assert_eq!(a.code(&["doctor"]), 0);
}

#[test]
fn retention_data_class_metrics_and_encryption() {
    // Retention: content of old messages is dropped, the chain stays.
    let mut e = Home::new("e", "eve", "default");
    #[allow(unused_mut)]
    e.env
        .push(("DIAVLOS_RETENTION_CHECK_SECS".into(), "1".into()));
    e.ok(&["new", "keep", "--retention", "0"]);
    e.ok(&["send", "keep", "old news"]);
    std::thread::sleep(Duration::from_secs(3));
    let log = e.ok(&["read", "keep", "--since", "1"]);
    assert!(log.contains("(content removed)"), "{log}");
    let bundle = e.ok(&["export", "keep"]);
    let path = e.dir.join("keep.jsonl");
    std::fs::write(&path, &bundle).unwrap();
    let out = e.ok(&["verify", path.to_str().unwrap()]);
    assert!(out.contains("2 tombstones") && out.contains("OK:"), "{out}");
    // A legal hold stops retention.
    e.ok(&["hold", "keep", "--on"]);
    e.ok(&["send", "keep", "kept"]);
    std::thread::sleep(Duration::from_secs(3));
    let log = e.ok(&["read", "keep", "--since", "1"]);
    assert!(log.contains("kept"), "{log}");

    // Data class: the helper refuses what it is told to.
    let f = Home::with_config(
        "f",
        "frank",
        "default",
        "[helper]\npublic_relays = false\nrefuse_classes = [\"pii\"]\n",
    );
    f.ok(&["new", "r2"]);
    f.fails_with(&["send", "r2", "ssn 123", "--class", "pii"], 6);
    f.ok(&["send", "r2", "fine", "--class", "internal"]);

    // Metrics on localhost for Prometheus.
    let port = 20000 + (nanos().len() as u16 % 1000) + (std::process::id() % 5000) as u16;
    let g = Home::with_config(
        "g",
        "gus",
        "default",
        &format!("[helper]\npublic_relays = false\nmetrics_addr = \"127.0.0.1:{port}\"\n"),
    );
    g.ok(&["new", "m"]);
    g.ok(&["send", "m", "x"]);
    let body = std::net::TcpStream::connect(("127.0.0.1", port))
        .and_then(|mut s| {
            use std::io::{Read, Write};
            s.write_all(b"GET /metrics HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
            let mut out = String::new();
            s.read_to_string(&mut out)?;
            Ok(out)
        })
        .expect("metrics listener");
    assert!(body.contains("diavlos_messages_total"), "{body}");

    // The inbox is encrypted at rest: content bodies are sealed.
    let st = g.json(&["status", "--json"]);
    assert_eq!(st["encrypted_inbox"], true);
    let db = std::fs::read(g.dir.join("diavlos.db")).unwrap();
    let wal = std::fs::read(g.dir.join("diavlos.db-wal")).unwrap_or_default();
    let all = [db, wal].concat();
    let text = String::from_utf8_lossy(&all);
    assert!(
        !text.contains("\"text\":\"x\""),
        "plain content found in the inbox file"
    );
}
