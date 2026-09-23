//! Regression tests for the gaps an outside review found in the safety
//! rails, run against the real binary. Each one first failed the way the
//! review said it would; each one now has to keep failing to get through.
//!
//! One helper per test, two keys on it: the person (`default`, human) and
//! an agent (`claude-code`), let in with an invite the way the docs say.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};

const AGENT: &str = "claude-code";
const ACTION: &str = r#"{"verb":"deploy","target":"prod","params":{}}"#;

struct Home {
    dir: PathBuf,
}

impl Home {
    fn new(tag: &str) -> Home {
        // Short on purpose: the helper's socket path lives inside it.
        let dir = std::env::temp_dir().join(format!("dvs-{tag}-{}", nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), "[helper]\npublic_relays = false\n").unwrap();
        Home { dir }
    }

    fn cmd(&self, who: &str, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_diavlos"));
        c.env("DIAVLOS_HOME", &self.dir)
            .env("USER", "haris")
            .env("USERNAME", "haris")
            .env("DIAVLOS_AS", who)
            .args(args);
        c
    }

    fn run(&self, who: &str, args: &[&str]) -> Output {
        self.cmd(who, args).output().expect("run diavlos")
    }

    fn ok(&self, who: &str, args: &[&str]) -> String {
        let out = self.run(who, args);
        assert!(
            out.status.success(),
            "diavlos {args:?} as {who} failed: {:?}\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A room owned by the person, with the agent let in as a member.
    fn room_with_agent(&self, room: &str) {
        self.room_with_agent_as(room, &[]);
    }

    /// The same, with extra invite flags, e.g. `--human`.
    fn room_with_agent_as(&self, room: &str, flags: &[&str]) {
        self.ok("default", &["new", room]);
        let mut args = vec!["invite", room, AGENT];
        args.extend_from_slice(flags);
        let note = self.ok("default", &args);
        let inv = note
            .split_whitespace()
            .find(|w| w.starts_with("dv1."))
            .expect("invite token")
            .to_string();
        self.ok(AGENT, &["join", &inv]);
    }

    /// Put a question carrying `ACTION` in the room and return its id. The
    /// asker is left waiting in the background; the caller kills it.
    fn pending_question(&self, room: &str) -> (String, Child) {
        let asker = self
            .cmd(
                AGENT,
                &[
                    "ask",
                    room,
                    "ship it?",
                    "--action",
                    ACTION,
                    "--timeout",
                    "60",
                ],
            )
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(200));
            let out = self.ok("default", &["read", room, "--since", "1", "--json"]);
            for line in out.lines().filter(|l| !l.trim().is_empty()) {
                let m: Value = serde_json::from_str(line).unwrap();
                if m["type"] == "question" {
                    return (m["id"].as_str().unwrap().to_string(), asker);
                }
            }
        }
        let mut asker = asker;
        let _ = asker.kill();
        let _ = asker.wait();
        panic!("the question never arrived");
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.run("default", &["stop"]);
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
            % 0xffff_ffff_ffff
    )
}

/// `diavlos mcp` over stdio, spoken to the way an agent's tool speaks to it.
struct Mcp {
    child: Child,
    lines: mpsc::Receiver<String>,
    next_id: u64,
}

impl Mcp {
    fn start(home: &Home, who: &str) -> Mcp {
        Mcp::try_start(home, who).expect("diavlos mcp did not answer initialize")
    }

    /// `None` if the server exits or never answers `initialize`.
    fn try_start(home: &Home, who: &str) -> Option<Mcp> {
        let mut child = home
            .cmd(who, &["mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut mcp = Mcp {
            child,
            lines: rx,
            next_id: 1,
        };
        mcp.try_request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "safety-test", "version": "0"}}),
        )?;
        mcp.notify("notifications/initialized");
        Some(mcp)
    }

    fn write(&mut self, v: Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str) {
        self.write(json!({"jsonrpc": "2.0", "method": method}));
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.try_request(method, params)
            .expect("no answer from diavlos mcp")
    }

    fn try_request(&mut self, method: &str, params: Value) -> Option<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let stdin = self.child.stdin.as_mut()?;
        writeln!(stdin, "{msg}").ok()?;
        stdin.flush().ok()?;
        loop {
            // A closed channel means the server exited.
            let line = self.lines.recv_timeout(Duration::from_secs(30)).ok()?;
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if v["id"] == id {
                    return Some(v);
                }
            }
        }
    }

    /// Call a tool; returns (is_error, the JSON body of its first text block).
    fn tool(&mut self, name: &str, args: Value) -> (bool, Value) {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}));
        let result = &r["result"];
        let text = result["content"][0]["text"].as_str().unwrap_or("{}");
        (
            result["isError"].as_bool().unwrap_or(false),
            serde_json::from_str(text).unwrap_or(json!({"raw": text})),
        )
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The review's reproduction, run end to end: an agent on MCP, running as
/// the default key, answers a pending deploy question with an approve. Before
/// the fix the helper signed it with the person's key and `check-approve`
/// let the deploy through. Now that MCP server refuses to start, and the
/// deploy is not allowed.
#[test]
fn the_reviewed_bypass_is_closed() {
    let h = Home::new("repro");
    h.room_with_agent("ops");
    let (qid, mut asker) = h.pending_question("ops");

    if let Some(mut mcp) = Mcp::try_start(&h, "default") {
        mcp.tool(
            "diavlos_send",
            json!({"room": "ops", "text": "approved", "type": "approve", "reply_to": qid}),
        );
    }

    let out = h.run("default", &["check-approve", "ops", ACTION]);
    assert_eq!(
        out.status.code(),
        Some(6),
        "an agent approved its own deploy: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let _ = asker.kill();
    let _ = asker.wait();
}

/// Even an MCP session whose member may approve cannot send an approve
/// through MCP. Here the agent's key was let in with `--human`, so the
/// helper alone would accept it: only the MCP boundary stands in the way.
#[test]
fn mcp_refuses_approve_deny_control_and_system() {
    let h = Home::new("allow");
    h.room_with_agent_as("ops", &["--human"]);
    let (qid, mut asker) = h.pending_question("ops");

    let mut mcp = Mcp::start(&h, AGENT);
    for kind in ["approve", "deny", "control", "system"] {
        let (is_error, body) = mcp.tool(
            "diavlos_send",
            json!({"room": "ops", "text": "yes", "type": kind, "reply_to": qid}),
        );
        assert!(is_error, "{kind} went through: {body}");
        assert_eq!(body["code"], 6, "{kind}: {body}");
    }
    // Ordinary work still flows.
    let (is_error, body) = mcp.tool(
        "diavlos_send",
        json!({"room": "ops", "text": "on it", "type": "reply", "reply_to": qid}),
    );
    assert!(!is_error, "{body}");
    drop(mcp);

    // And the gate says no.
    let out = h.run("default", &["check-approve", "ops", ACTION]);
    assert_eq!(
        out.status.code(),
        Some(6),
        "check-approve allowed it: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let _ = asker.kill();
    let _ = asker.wait();
}

/// `diavlos mcp` will not run as a human key: not as `default`, and not as a
/// human key under any other label. The check is the key's kind.
#[test]
fn mcp_refuses_a_human_key_whatever_it_is_called() {
    let h = Home::new("kind");
    h.room_with_agent("ops");

    let out = h.run("default", &["mcp"]);
    assert!(!out.status.success(), "mcp served as default");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("human key"), "{err}");
    assert!(
        err.contains("diavlos --as <agent-name> join"),
        "no way forward in: {err}"
    );

    // The same human key under another name.
    let keys = h.dir.join("keys");
    std::fs::copy(keys.join("default.json"), keys.join("boss.json")).unwrap();
    let out = h.run("boss", &["mcp"]);
    assert!(!out.status.success(), "mcp served as a renamed human key");
    assert!(String::from_utf8_lossy(&out.stderr).contains("human key"));

    // The agent's own key is fine.
    let mut mcp = Mcp::start(&h, AGENT);
    let (is_error, body) = mcp.tool("diavlos_rooms", json!({}));
    assert!(!is_error, "{body}");
}

/// A key inside an action's params used to pass the secret scan and be
/// signed and replicated. The whole payload is scanned now.
#[test]
fn a_secret_inside_an_action_is_refused() {
    let h = Home::new("scan");
    h.room_with_agent("ops");
    let key = "AKIAIOSFODNN7EXAMPLE";
    let action = format!(r#"{{"verb":"deploy","target":"prod","params":{{"aws_key":"{key}"}}}}"#);

    let out = h.run(
        AGENT,
        &[
            "ask",
            "ops",
            "deploy?",
            "--action",
            &action,
            "--timeout",
            "3",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(6),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("AWS access key"));

    // Nothing carrying it was stored.
    let log = h.ok("default", &["read", "ops", "--since", "1", "--json"]);
    assert!(!log.contains(key), "the key reached the room:\n{log}");
}
