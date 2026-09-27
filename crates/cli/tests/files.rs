//! Files on messages, between two helpers on one machine with public
//! relays off: the upload to the room's home in signed pieces, the
//! reader's fetch and check, the safe folder, and the room's rules
//! (files off, the safe list, the size limit). See docs/FILES.md.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Home {
    dir: PathBuf,
    user: &'static str,
}

impl Home {
    fn new(tag: &str, user: &'static str) -> Home {
        let dir = std::env::temp_dir().join(format!("diavlos-files-{tag}-{}", nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "[helper]\npublic_relays = false\nretry_secs = 1\nkeychain = false\n",
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

    fn helper_log(&self) -> String {
        let text = std::fs::read_to_string(self.dir.join("helper.log")).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(40)..].join("\n")
    }

    /// A file of our own to send, outside the diavlos home.
    fn file(&self, name: &str, bytes: &[u8]) -> String {
        let dir = self.dir.join("mine");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p.to_string_lossy().to_string()
    }

    fn policy(&self, room: &str, text: &str) {
        let dir = self.dir.join("rooms").join(room);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("policy.toml"), text).unwrap();
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.run(&["stop"]);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn invite_token(note: &str) -> String {
    note.split_whitespace()
        .find(|w| w.starts_with("dv1."))
        .expect("invite token in note")
        .to_string()
}

/// Room `ops` on `a` (the home), with bob on `b`.
fn pair(tag: &str) -> (Home, Home) {
    let a = Home::new(&format!("{tag}-a"), "haris");
    let b = Home::new(&format!("{tag}-b"), "bobhost");
    a.ok(&["new", "ops"]);
    let inv = invite_token(&a.ok(&["invite", "ops", "bob"]));
    b.ok(&["--as", "fixer", "join", &inv]);
    (a, b)
}

fn saved(out: &str) -> Vec<serde_json::Value> {
    serde_json::from_str(out.trim()).unwrap()
}

fn is_private(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777 == 0o600
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

#[test]
fn a_file_goes_to_the_home_in_pieces_and_comes_back_checked() {
    let (a, b) = pair("trip");
    // Over 2 MiB, so it takes three pieces each way.
    let big: Vec<u8> = (0..2_500_000u32).map(|i| (i % 251) as u8).collect();
    let path = b.file("dump.bin", &big);
    let out = b.ok(&[
        "--as", "fixer", "send", "ops", "the dump", "--file", &path, "--json",
    ]);
    let msg: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert!(msg["seq"].as_u64().unwrap() > 0, "not delivered: {out}");
    let f = &msg["data"]["files"][0];
    assert_eq!(f["name"], "dump.bin");
    assert_eq!(f["size"], big.len() as u64);
    let id = f["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("sha256:"));

    // The home's reader sees the file on the message and fetches it.
    let got = a.ok(&["next", "ops", "--timeout", "20"]);
    assert!(
        got.contains("file sha256:") && got.contains("dump.bin"),
        "{got}"
    );
    let s = saved(&a.ok(&["get", "ops", &id, "--json"]));
    let p = PathBuf::from(s[0]["path"].as_str().unwrap());
    assert_eq!(std::fs::read(&p).unwrap(), big);
    assert!(p.starts_with(a.dir.join("files").join("ops")), "{p:?}");
    assert!(is_private(&p));
    assert_eq!(s[0]["from"], "bob");

    // The other way: the home sends, bob fetches over the link, by the
    // message id and by the start of the file id.
    let notes = a.file("notes.txt", "καλημέρα\nfixed in 4f2a\n".as_bytes());
    let out = a.ok(&["send", "ops", "notes", "--file", &notes, "--json"]);
    let m: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    let mid = m["id"].as_str().unwrap().to_string();
    let fid = m["data"]["files"][0]["id"].as_str().unwrap().to_string();
    b.ok(&["--as", "fixer", "next", "ops", "--timeout", "20"]);
    let s = saved(&b.ok(&["--as", "fixer", "get", "ops", &mid, "--json"]));
    assert_eq!(s[0]["kind"], "text");
    assert!(s[0]["warnings"].as_array().unwrap().is_empty(), "{s:?}");
    let first = PathBuf::from(s[0]["path"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(&first).unwrap(),
        "καλημέρα\nfixed in 4f2a\n"
    );
    // Fetching it again reuses the same saved file.
    let short = &fid["sha256:".len().."sha256:".len() + 12];
    let s = saved(&b.ok(&["--as", "fixer", "get", "ops", short, "--json"]));
    assert_eq!(PathBuf::from(s[0]["path"].as_str().unwrap()), first);

    // Nothing to fetch: a message with no files, a file nobody sent.
    b.fails_with(&["--as", "fixer", "get", "ops", "m_nothing"], 1);
    b.fails_with(&["--as", "fixer", "get", "ops", &"0".repeat(64)], 1);
}

#[test]
fn a_runnable_file_is_saved_so_it_does_not_run_and_the_reader_is_warned() {
    let (a, b) = pair("warn");
    let script = b.file("fix.sh", b"#!/bin/sh\necho hi\n");
    let pic = b.file("photo.png", b"MZ\x90\x00 not a picture");
    let out = b.ok(&[
        "--as", "fixer", "send", "ops", "run this", "--file", &script, "--file", &pic, "--json",
    ]);
    let m: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    let s = saved(&a.ok(&["get", "ops", m["id"].as_str().unwrap(), "--json"]));
    assert_eq!(s.len(), 2);
    assert!(
        s[0]["path"].as_str().unwrap().ends_with("fix.sh.unsafe"),
        "{s:?}"
    );
    assert_eq!(s[0]["kind"], "program");
    let w = s[1]["warnings"].to_string();
    assert!(
        w.contains("run as a program") && w.contains("not image/png"),
        "{w}"
    );
    assert!(is_private(Path::new(s[0]["path"].as_str().unwrap())));
    // The plain-text view prints the warnings too.
    let text = a.ok(&["get", "ops", m["id"].as_str().unwrap()]);
    assert!(
        text.contains("warning: this file can run as a program"),
        "{text}"
    );
}

#[test]
fn the_room_decides_which_files_and_how_big() {
    let (a, b) = pair("rules");
    let script = b.file("x.sh", b"#!/bin/sh\n");
    let text = b.file("ok.txt", b"plain words\n");

    // The safe list: text goes, a program does not.
    a.policy("ops", "files = \"safe\"\n");
    let err = b.fails_with(&["--as", "fixer", "send", "ops", "s", "--file", &script], 6);
    assert!(err.contains("only plain text"), "{err}");
    b.ok(&["--as", "fixer", "send", "ops", "t", "--file", &text]);

    // The size limit, told before any bytes go.
    a.policy("ops", "max_file_mb = 1\n");
    let big = b.file("big.log", &vec![b'x'; 1_100_000]);
    let err = b.fails_with(&["--as", "fixer", "send", "ops", "b", "--file", &big], 6);
    assert!(err.contains("up to 1 MB"), "{err}");

    // Off: no files at all.
    a.policy("ops", "files = \"off\"\n");
    let err = b.fails_with(&["--as", "fixer", "send", "ops", "o", "--file", &text], 6);
    assert!(err.contains("takes no files"), "{err}");

    // A folder is not a file.
    let dir = b.dir.join("mine").to_string_lossy().to_string();
    b.fails_with(&["--as", "fixer", "send", "ops", "d", "--file", &dir], 1);

    // Nobody fills in data.files by hand.
    a.policy("ops", "");
    let fake = format!(
        r#"{{"files":[{{"id":"sha256:{}","name":"a","size":1}}]}}"#,
        "a".repeat(64)
    );
    b.fails_with(&["--as", "fixer", "send", "ops", "f", "--data", &fake], 1);
}

#[test]
fn a_pii_room_takes_no_files_until_its_owner_says_so() {
    let a = Home::new("pii", "haris");
    a.ok(&["new", "hr", "--class", "pii"]);
    let doc = a.file("list.csv", b"name,email\n");
    let err = a.fails_with(&["send", "hr", "the list", "--file", &doc], 6);
    assert!(err.contains("takes no files"), "{err}");
    a.policy("hr", "files = \"safe\"\n");
    a.ok(&["send", "hr", "the list", "--file", &doc]);
}

#[test]
fn a_file_sent_while_the_home_is_away_waits_and_then_goes() {
    let (a, b) = pair("away");
    a.ok(&["stop"]);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let log = b.file("late.log", b"sent while you were out\n");
    let out = b.ok(&["--as", "fixer", "send", "ops", "late", "--file", &log]);
    assert!(out.starts_with("queued") && out.contains("files"), "{out}");
    // The sender's copy is what goes, so changing the original does nothing.
    std::fs::write(&log, b"changed after send\n").unwrap();
    a.ok(&["status"]);
    let got = a.ok(&["next", "ops", "--timeout", "30", "--json"]);
    let m: serde_json::Value = serde_json::from_str(got.trim()).unwrap();
    let s = saved(&a.ok(&["get", "ops", m["id"].as_str().unwrap(), "--json"]));
    assert_eq!(
        std::fs::read_to_string(s[0]["path"].as_str().unwrap()).unwrap(),
        "sent while you were out\n"
    );
}

#[test]
fn one_member_cannot_fill_the_room() {
    let (a, b) = pair("quota");
    a.policy("ops", "daily_file_mb_per_member = 1\n");
    let one = b.file("one.bin", &vec![1u8; 600_000]);
    let two = b.file("two.bin", &vec![2u8; 600_000]);
    b.ok(&["--as", "fixer", "send", "ops", "1", "--file", &one]);
    // The same bytes again cost nothing: the home already has them.
    b.ok(&["--as", "fixer", "send", "ops", "1 again", "--file", &one]);
    let err = b.fails_with(&["--as", "fixer", "send", "ops", "2", "--file", &two], 1);
    assert!(err.contains("in the last 24 hours"), "{err}");
    // The owner is not held to bob's share.
    a.ok(&["send", "ops", "mine", "--file", &two]);

    a.policy("ops", "room_file_store_mb = 1\n");
    let three = b.file("three.bin", &vec![3u8; 600_000]);
    let err = b.fails_with(&["--as", "fixer", "send", "ops", "3", "--file", &three], 6);
    assert!(err.contains("file store is full"), "{err}");
}

#[test]
fn a_text_file_with_a_secret_in_it_is_not_sent() {
    let a = Home::new("secret", "haris");
    a.ok(&["new", "ops"]);
    let key = a.file(
        "creds.txt",
        b"-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----\n",
    );
    let err = a.fails_with(&["send", "ops", "keys", "--file", &key], 6);
    assert!(err.contains("creds.txt looks like it holds"), "{err}");
}
