//! Private network only: two helpers that may talk only inside one range
//! still trade messages, and a helper with no address in its range says so
//! instead of quietly using the open internet. Loopback stands in for the
//! VPN, so the test needs no network.

use std::path::PathBuf;
use std::process::{Command, Output};

struct Home {
    dir: PathBuf,
    user: &'static str,
}

impl Home {
    fn new(tag: &str, user: &'static str, networks: &str) -> Home {
        let dir = std::env::temp_dir().join(format!("diavlos-private-{tag}-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            format!("[helper]\nretry_secs = 1\nprivate_networks = [{networks}]\n"),
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
            std::fs::read_to_string(self.dir.join("helper.log")).unwrap_or_default()
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.run(&["stop"]);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn stamp() -> String {
    format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

#[test]
fn two_helpers_talk_inside_the_private_network() {
    let a = Home::new("a", "haris", "\"127.0.0.0/8\"");
    let b = Home::new("b", "bobhost", "\"127.0.0.0/8\"");

    a.ok(&["new", "ops", "--about", "test"]);
    let note = a.ok(&["invite", "ops", "bob"]);
    let inv = note
        .split_whitespace()
        .find(|w| w.starts_with("dv1."))
        .expect("invite token")
        .to_string();
    b.ok(&["join", &inv]);

    b.ok(&["send", "ops", "over the tunnel", "--type", "task"]);
    let got = a.ok(&["next", "ops", "--timeout", "20"]);
    assert!(got.contains("bob (task): over the tunnel"), "{got}");

    // The helper advertises only its address inside the range.
    let status = a.ok(&["status", "--json"]);
    assert!(status.contains("127.0.0.1"), "{status}");
    assert!(!status.contains("relay.iroh"), "{status}");

    let doctor = a.ok(&["doctor", "--json"]);
    assert!(doctor.contains("private network"), "{doctor}");
}

#[test]
fn no_address_in_the_range_is_said_plainly() {
    // TEST-NET-3 is never a local address.
    let a = Home::new("none", "haris", "\"203.0.113.0/24\"");
    let out = a.run(&["new", "ops"]);
    assert!(!out.status.success());
    let log = std::fs::read_to_string(a.dir.join("helper.log")).unwrap_or_default();
    let all = format!(
        "{}{}{log}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(all.contains("is the VPN up?"), "{all}");

    let doctor = a.run(&["doctor", "--json"]);
    let text = String::from_utf8_lossy(&doctor.stdout);
    assert!(text.contains("no address in 203.0.113.0/24"), "{text}");
}
