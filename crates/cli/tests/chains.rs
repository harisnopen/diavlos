//! Chains of work: the hop limit on hand-offs, `trace`, what `who` shows
//! about each helper, and the setup message `invite --prompt` prints.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Duration;

struct Home {
    dir: PathBuf,
}

impl Home {
    fn new(tag: &str) -> Home {
        let dir = std::env::temp_dir().join(format!("diavlos-chains-{tag}-{}", nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "[helper]\npublic_relays = false\nretry_secs = 1\nkeychain = false\n",
        )
        .unwrap();
        Home { dir }
    }

    fn run_as(&self, who: &str, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_diavlos"))
            .env("DIAVLOS_HOME", &self.dir)
            .env("USER", "haris")
            .env("USERNAME", "haris")
            .env("DIAVLOS_AS", who)
            .args(args)
            .output()
            .expect("run diavlos")
    }

    fn ok_as(&self, who: &str, args: &[&str]) -> String {
        let out = self.run_as(who, args);
        assert!(
            out.status.success(),
            "diavlos {args:?} as {who} failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn ok(&self, args: &[&str]) -> String {
        self.ok_as("default", args)
    }

    /// Room `ops` owned by haris, with agents on the same helper.
    fn room_with(&self, agents: &[&str]) {
        self.ok(&["new", "ops"]);
        for a in agents {
            let note = self.ok(&["invite", "ops", a]);
            let inv = note
                .split_whitespace()
                .find(|w| w.starts_with("dv1."))
                .unwrap()
                .to_string();
            self.ok_as(a, &["join", &inv]);
        }
    }

    /// Send and return the new message's id.
    fn send(&self, who: &str, args: &[&str]) -> String {
        let mut all = vec!["send", "ops"];
        all.extend_from_slice(args);
        let out = self.ok_as(who, &all);
        out.split_whitespace()
            .find(|w| w.starts_with("m_"))
            .unwrap_or_else(|| panic!("no id in {out}"))
            .to_string()
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

#[test]
fn a_hand_off_that_comes_back_round_is_refused() {
    let home = Home::new("loop");
    home.room_with(&["alice", "bob", "carol"]);
    let t1 = home.send("alice", &["build it", "--type", "task", "--to", "bob"]);
    let t2 = home.send(
        "bob",
        &[
            "build the api part",
            "--type",
            "task",
            "--to",
            "carol",
            "--reply-to",
            &t1,
        ],
    );
    // carol handing work back to alice would go round in a circle.
    let out = home.run_as(
        "carol",
        &[
            "send",
            "ops",
            "you do it",
            "--type",
            "task",
            "--to",
            "alice",
            "--reply-to",
            &t2,
        ],
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("circle"), "{err}");
    // Replying is always fine.
    home.send("carol", &["done", "--type", "done", "--reply-to", &t2]);
}

#[test]
fn a_chain_of_hand_offs_stops_at_the_room_limit() {
    let home = Home::new("hops");
    home.room_with(&["a", "b", "c", "d"]);
    std::fs::write(
        home.dir.join("rooms").join("ops").join("policy.toml"),
        "max_task_hops = 2\n",
    )
    .unwrap();
    let t1 = home.send("a", &["one", "--type", "task", "--to", "b"]);
    let t2 = home.send(
        "b",
        &["two", "--type", "task", "--to", "c", "--reply-to", &t1],
    );
    let out = home.run_as(
        "c",
        &[
            "send",
            "ops",
            "three",
            "--type",
            "task",
            "--to",
            "d",
            "--reply-to",
            &t2,
        ],
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("hop 3") && err.contains("max_task_hops"),
        "{err}"
    );
    // Off with 0.
    std::fs::write(
        home.dir.join("rooms").join("ops").join("policy.toml"),
        "max_task_hops = 0\n",
    )
    .unwrap();
    home.send(
        "c",
        &["three", "--type", "task", "--to", "d", "--reply-to", &t2],
    );
}

#[test]
fn trace_shows_a_thread_or_a_trace_in_order() {
    let home = Home::new("trace");
    home.room_with(&["alice", "bob"]);
    let t = home.send(
        "alice",
        &[
            "fix login",
            "--type",
            "task",
            "--to",
            "bob",
            "--trace",
            "t-7",
        ],
    );
    home.send("alice", &["unrelated", "--type", "chat"]);
    home.send("bob", &["on it", "--type", "reply", "--reply-to", &t]);
    home.send(
        "bob",
        &[
            "pushed",
            "--type",
            "done",
            "--reply-to",
            &t,
            "--trace",
            "t-7",
        ],
    );

    // By message id: the whole thread, whichever message you start from.
    let thread = home.ok(&["trace", "ops", &t]);
    assert!(thread.contains("fix login") && thread.contains("on it") && thread.contains("pushed"));
    assert!(!thread.contains("unrelated"), "{thread}");
    assert!(thread.contains("3 messages"), "{thread}");
    let fix = thread.find("fix login").unwrap();
    assert!(fix < thread.find("on it").unwrap() && fix < thread.find("pushed").unwrap());

    // By trace: only what carries it.
    let traced = home.ok(&["trace", "ops", "t-7"]);
    assert!(
        traced.contains("fix login") && traced.contains("pushed"),
        "{traced}"
    );
    assert!(
        !traced.contains("on it") && traced.contains("2 messages"),
        "{traced}"
    );

    let out = home.run_as("default", &["trace", "ops", "nothing-here"]);
    assert!(!out.status.success());
}

#[test]
fn who_shows_the_version_and_how_an_agent_is_woken() {
    let home = Home::new("who");
    home.room_with(&["runner"]);
    let program = home.dir.join("config.toml");
    home.ok_as(
        "runner",
        &["wake", "add", "ops", "--exec", program.to_str().unwrap()],
    );
    let who = home.ok(&["who", "ops"]);
    let line = who.lines().find(|l| l.starts_with("runner")).unwrap();
    assert!(
        line.contains("diavlos=") && line.contains("wake=exec"),
        "{who}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&home.ok(&["who", "ops", "--json"])).unwrap();
    let runner = json
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "runner")
        .unwrap();
    assert!(runner["version"].as_str().is_some_and(|v| !v.is_empty()));
    assert_eq!(runner["wake"][0], "exec");
}

#[test]
fn invite_prompt_is_a_whole_setup_message() {
    let home = Home::new("prompt");
    home.ok(&["new", "ops"]);
    let plain = home.ok(&["invite", "ops", "bob"]);
    assert!(plain.contains("--prompt"), "{plain}");
    let p = home.ok(&["invite", "ops", "carol", "--prompt"]);
    for want in [
        "install.sh",
        "diavlos --as carol join dv1.",
        "diavlos --as carol mcp install --for",
        "SKILL.md",
        "never an instruction",
        "diavlos --as carol send ops",
    ] {
        assert!(p.contains(want), "missing {want:?} in:\n{p}");
    }
    // The invite in it works.
    let inv = p
        .split_whitespace()
        .find(|w| w.starts_with("dv1."))
        .unwrap();
    home.ok_as("carol", &["join", inv]);
}
