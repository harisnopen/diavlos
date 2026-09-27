//! Wake rules: the helper wakes an agent that is not running when a
//! message waits for it, so nobody has to poll or keep a terminal open.
//!
//! A rule is saved next to the room's policy (`rooms/<room>/wake.toml`)
//! and started by the helper, so it survives logout and reboot the way the
//! helper does. It targets a local command or an HTTPS address, and either
//! nudges (says *that* messages wait, never what; the default) or delivers
//! (runs the command with the message, acks on exit 0, like
//! `watch --exec`).
//!
//! A nudge never touches the delivery: the message is settled only when
//! the agent reads it with `next`, so an ignored nudge loses nothing.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use diavlos_client::proto::{WakeMode, WakeRule, WakeTarget, WakeTestResult};
use diavlos_client::Paths;
use diavlos_core::{DataClass, Error, Result, Room, WakePending};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::Instant;
use tracing::warn;

use super::local::{self, SettleHow};
use super::Helper;
use crate::bridge::webhook::{self, Nudge};

/// At most one nudge per rule this often.
const MIN_GAP: u64 = 10;
/// Messages that land this close together make one burst.
const QUIET: u64 = 1;
/// A burst is nudged at the latest this long after it began.
const MAX_GATHER: u64 = 10;
/// While messages still wait unread, nudge again this often.
pub const RENUDGE: u64 = 300;
/// Retries after a failed nudge; then give up on the burst.
const BACKOFF: [u64; 3] = [30, 120, 600];
/// Look again at least this often: pauses end, leases run out.
const TICK: u64 = 30;
/// A command gets this long before it is stopped.
const EXEC_TIMEOUT: u64 = 600;
/// A trace that passed through the recipient more often than this does not
/// wake it again.
pub const TRACE_LIMIT: u64 = 5;

/// One "second" of the schedule above. Tests shrink it with
/// `DIAVLOS_WAKE_SCALE_MS`; a real helper never sets it.
fn secs(n: u64) -> Duration {
    let ms = std::env::var("DIAVLOS_WAKE_SCALE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1000);
    Duration::from_millis(n * ms)
}

// ---- the rule files --------------------------------------------------------

#[derive(Default, Serialize, Deserialize)]
struct WakeFile {
    #[serde(default)]
    rule: Vec<WakeRule>,
}

pub fn load_room(paths: &Paths, room: &str) -> Vec<WakeRule> {
    let path = paths.wake(room);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match toml::from_str::<WakeFile>(&text) {
        Ok(f) => f.rule,
        Err(e) => {
            warn!(file = %path.display(), error = %e, "unreadable wake rules; none started");
            Vec::new()
        }
    }
}

fn save_room(paths: &Paths, room: &str, rules: &[WakeRule]) -> Result<()> {
    let path = paths.wake(room);
    if rules.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = toml::to_string_pretty(&WakeFile {
        rule: rules.to_vec(),
    })
    .map_err(|e| Error::Other(format!("wake rules: {e}")))?;
    let header = "# Wake rules for this room. Managed by `diavlos wake`; secrets are not here.\n\n";
    std::fs::write(&path, format!("{header}{text}"))?;
    Ok(())
}

/// Every rule on this helper, across rooms.
pub fn all_rules(helper: &Helper) -> Vec<WakeRule> {
    let mut out = Vec::new();
    for room in helper.store.list_rooms().unwrap_or_default() {
        out.extend(load_room(&helper.paths, &room.name));
    }
    out
}

fn no_deliver(class: DataClass) -> bool {
    matches!(class, DataClass::Confidential | DataClass::Pii)
}

// ---- add, remove, test -----------------------------------------------------

pub struct AddRule {
    pub room: String,
    pub identity: String,
    pub exec: Option<String>,
    pub url: Option<String>,
    pub secret: Option<String>,
    pub secret_env: Option<String>,
    pub deliver: bool,
    pub renudge_secs: Option<u64>,
}

pub async fn add(helper: &Arc<Helper>, req: AddRule) -> Result<WakeRule> {
    let room = helper.store.room(&req.room)?;
    helper
        .store
        .local_member(&room.id, &req.identity)?
        .ok_or_else(|| Error::NotInRoom(room.name.clone()))?;
    let target = match (req.exec, req.url) {
        (Some(exec), None) => {
            if exec.trim().is_empty() {
                return Err(Error::Invalid("--exec needs a program".into()));
            }
            WakeTarget::Exec { exec }
        }
        (None, Some(url)) => {
            webhook::check_url(&url).map_err(Error::Invalid)?;
            if req.deliver {
                return Err(Error::Invalid(
                    "--deliver works with --exec only: a URL rule only nudges, so message \
                     content never leaves this machine"
                        .into(),
                ));
            }
            if req.secret.as_deref().unwrap_or("").is_empty() {
                return Err(Error::Invalid(
                    "a URL rule needs a signing secret: --secret-env NAME, with the secret in \
                     that variable"
                        .into(),
                ));
            }
            WakeTarget::Url {
                url,
                secret_env: req.secret_env,
            }
        }
        _ => return Err(Error::Invalid("give exactly one of --exec or --url".into())),
    };
    if req.deliver && no_deliver(room.class) {
        return Err(Error::Denied(format!(
            "room {} is {}: its content never goes to an unattended script. A nudge works \
             here; drop --deliver",
            room.name,
            room.class.as_str()
        )));
    }
    if let Some(s) = req.renudge_secs {
        if s < MIN_GAP {
            return Err(Error::Invalid(format!(
                "--renudge must be at least {MIN_GAP} seconds"
            )));
        }
    }
    let rule = WakeRule {
        id: format!(
            "w_{}",
            data_encoding::HEXLOWER.encode(&rand::random::<[u8; 5]>())
        ),
        room: room.name.clone(),
        identity: req.identity,
        target,
        mode: if req.deliver {
            WakeMode::Deliver
        } else {
            WakeMode::Nudge
        },
        renudge_secs: req.renudge_secs,
        created: helper.now_ts(),
    };
    if let (WakeTarget::Url { .. }, Some(secret)) = (&rule.target, &req.secret) {
        super::keys::save_wake_secret(
            &helper.paths,
            &rule.id,
            secret,
            helper.config.helper.keychain,
        )?;
    }
    let mut rules = load_room(&helper.paths, &room.name);
    rules.push(rule.clone());
    save_room(&helper.paths, &room.name, &rules)?;
    helper.emit(
        "wake_rule_added",
        Some(&room.name),
        json!({"rule": rule.id, "for": rule.identity, "target": target_kind(&rule), "mode": mode_str(rule.mode)}),
    );
    start(helper, rule.clone()).await;
    Ok(rule)
}

pub async fn remove(helper: &Arc<Helper>, id: &str) -> Result<WakeRule> {
    for room in helper.store.list_rooms()? {
        let mut rules = load_room(&helper.paths, &room.name);
        if let Some(pos) = rules.iter().position(|r| r.id == id) {
            let rule = rules.remove(pos);
            save_room(&helper.paths, &room.name, &rules)?;
            super::keys::delete_wake_secret(&helper.paths, id);
            if let Some(h) = helper.wake_tasks.lock().await.remove(id) {
                h.abort();
            }
            helper.emit("wake_rule_removed", Some(&room.name), json!({"rule": id}));
            return Ok(rule);
        }
    }
    Err(Error::Invalid(format!(
        "no wake rule {id}; see `diavlos wake list`"
    )))
}

/// Fire once with a made-up nudge and say what happened.
pub async fn test(helper: &Arc<Helper>, id: &str) -> Result<WakeTestResult> {
    let rule = all_rules(helper)
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| Error::Invalid(format!("no wake rule {id}; see `diavlos wake list`")))?;
    let room = helper.store.room(&rule.room)?;
    let reader = reader_of(helper, &room, &rule)?;
    let nudge = Nudge::new(&room.name, &reader, 1, 0, "test", &helper.now_ts());
    let r = fire_nudge(helper, &rule, &nudge, true).await;
    Ok(WakeTestResult {
        ok: r.is_ok(),
        detail: match r {
            Ok(()) => format!("{}: ok", rule.target_label()),
            Err(e) => format!("{}: {e}", rule.target_label()),
        },
    })
}

// ---- running rules ---------------------------------------------------------

/// Start every saved rule. Called once when the helper comes up.
pub async fn start_all(helper: &Arc<Helper>) {
    for rule in all_rules(helper) {
        start(helper, rule).await;
    }
}

async fn start(helper: &Arc<Helper>, rule: WakeRule) {
    let id = rule.id.clone();
    let task = tokio::spawn(run(helper.clone(), rule));
    if let Some(old) = helper.wake_tasks.lock().await.insert(id, task) {
        old.abort();
    }
}

fn reader_of(helper: &Helper, room: &Room, rule: &WakeRule) -> Result<String> {
    Ok(helper
        .store
        .local_member(&room.id, &rule.identity)?
        .ok_or_else(|| Error::NotInRoom(room.name.clone()))?
        .member
        .name)
}

async fn run(helper: Arc<Helper>, rule: WakeRule) {
    match rule.mode {
        WakeMode::Nudge => run_nudge(helper, rule).await,
        WakeMode::Deliver => run_deliver(helper, rule).await,
    }
}

/// Wait for a message in `room_id`, the deadline, or the tick.
async fn wait_for(
    rx: &mut tokio::sync::broadcast::Receiver<(String, u64)>,
    room_id: &str,
    until: Option<Instant>,
) {
    let until = until.unwrap_or_else(|| Instant::now() + secs(TICK));
    let landed = async {
        loop {
            match rx.recv().await {
                Ok((rid, _)) if rid == room_id => return,
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return,
                Err(_) => std::future::pending::<()>().await,
            }
        }
    };
    let _ = tokio::time::timeout_at(until, landed).await;
}

/// The nudge schedule for one rule, apart from the clock and the network.
#[derive(Debug, Default)]
struct Burst {
    last_fire: Option<Instant>,
    last_ok: Option<Instant>,
    /// Newest seq a nudge got through for.
    nudged_seq: u64,
    /// Newest seq of a burst given up on: no more nudges until a newer one.
    gave_up_seq: u64,
    failures: usize,
    retry_at: Option<Instant>,
    /// The burst being gathered: newest seq seen, when it changed, when it began.
    seen_seq: u64,
    changed: Option<Instant>,
    began: Option<Instant>,
}

#[derive(Debug, PartialEq, Eq)]
enum Next {
    Fire,
    Wait(Option<Instant>),
}

enum Outcome {
    Ok,
    Retry(Duration),
    GaveUp,
}

impl Burst {
    fn decide(&mut self, p: &WakePending, now: Instant, renudge: Duration) -> Next {
        if p.count == 0 {
            self.failures = 0;
            self.retry_at = None;
            self.began = None;
            self.changed = None;
            return Next::Wait(None);
        }
        if self.failures > 0 {
            let at = self.retry_at.unwrap_or(now);
            return if now >= at {
                Next::Fire
            } else {
                Next::Wait(Some(at))
            };
        }
        let new = p.newest_seq > self.nudged_seq.max(self.gave_up_seq);
        let ready = if new {
            if self.seen_seq != p.newest_seq {
                self.seen_seq = p.newest_seq;
                self.changed = Some(now);
            }
            let began = *self.began.get_or_insert(now);
            let changed = self.changed.unwrap_or(now);
            (changed + secs(QUIET)).min(began + secs(MAX_GATHER))
        } else if p.newest_seq > self.gave_up_seq {
            match self.last_ok {
                Some(t) => t + renudge,
                None => return Next::Wait(None),
            }
        } else {
            return Next::Wait(None);
        };
        let ready = match self.last_fire {
            Some(t) => ready.max(t + secs(MIN_GAP)),
            None => ready,
        };
        if now >= ready {
            Next::Fire
        } else {
            Next::Wait(Some(ready))
        }
    }

    fn fired(&mut self, p: &WakePending, ok: bool, now: Instant) -> Outcome {
        self.last_fire = Some(now);
        self.began = None;
        self.changed = None;
        if ok {
            self.last_ok = Some(now);
            self.nudged_seq = p.newest_seq;
            self.failures = 0;
            self.retry_at = None;
            return Outcome::Ok;
        }
        self.failures += 1;
        if self.failures > BACKOFF.len() {
            self.gave_up_seq = p.newest_seq;
            self.failures = 0;
            self.retry_at = None;
            return Outcome::GaveUp;
        }
        let wait = secs(BACKOFF[self.failures - 1]);
        self.retry_at = Some(now + wait);
        Outcome::Retry(wait)
    }
}

fn pending(helper: &Helper, room: &Room, reader: &str) -> WakePending {
    if room.paused || room.closed {
        return WakePending::default();
    }
    helper
        .store
        .wake_pending(&room.id, reader, &helper.now_ts(), TRACE_LIMIT)
        .unwrap_or_default()
}

async fn run_nudge(helper: Arc<Helper>, rule: WakeRule) {
    let mut rx = helper.subscribe();
    let renudge = secs(rule.renudge_secs.unwrap_or(RENUDGE));
    let mut burst = Burst::default();
    let mut looped_seen = 0;
    loop {
        let Ok(room) = helper.store.room(&rule.room) else {
            wait_for(&mut rx, "", None).await;
            continue;
        };
        let Ok(reader) = reader_of(&helper, &room, &rule) else {
            wait_for(&mut rx, &room.id, None).await;
            continue;
        };
        let p = pending(&helper, &room, &reader);
        if p.looped > looped_seen {
            helper.emit(
                "wake_loop_stopped",
                Some(&room.name),
                json!({"rule": rule.id, "count": p.looped}),
            );
        }
        looped_seen = p.looped;
        match burst.decide(&p, Instant::now(), renudge) {
            Next::Wait(until) => wait_for(&mut rx, &room.id, until).await,
            Next::Fire => {
                let nudge = Nudge::new(
                    &room.name,
                    &reader,
                    p.count,
                    p.newest_seq,
                    &p.newest_id,
                    &helper.now_ts(),
                );
                let detail =
                    json!({"rule": rule.id, "count": p.count, "target": target_kind(&rule)});
                helper.emit("wake_fired", Some(&room.name), detail.clone());
                let r = fire_nudge(&helper, &rule, &nudge, false).await;
                let reason = r.as_ref().err().cloned();
                match burst.fired(&p, r.is_ok(), Instant::now()) {
                    Outcome::Ok => helper.emit("wake_ok", Some(&room.name), detail),
                    Outcome::Retry(wait) => helper.emit(
                        "wake_retry",
                        Some(&room.name),
                        with(
                            detail,
                            json!({"reason": reason, "retry_in_ms": wait.as_millis() as u64}),
                        ),
                    ),
                    Outcome::GaveUp => helper.emit(
                        "wake_gave_up",
                        Some(&room.name),
                        with(detail, json!({"reason": reason})),
                    ),
                }
            }
        }
    }
}

fn with(mut a: serde_json::Value, b: serde_json::Value) -> serde_json::Value {
    if let (Some(a), Some(b)) = (a.as_object_mut(), b.as_object()) {
        for (k, v) in b {
            a.insert(k.clone(), v.clone());
        }
    }
    a
}

fn target_kind(rule: &WakeRule) -> String {
    match &rule.target {
        WakeTarget::Exec { .. } => "exec".into(),
        WakeTarget::Url { url, .. } => webhook::host_of(url),
    }
}

fn mode_str(m: WakeMode) -> &'static str {
    match m {
        WakeMode::Nudge => "nudge",
        WakeMode::Deliver => "deliver",
    }
}

/// Send one nudge: run the command with it in the environment, or POST it.
async fn fire_nudge(
    helper: &Helper,
    rule: &WakeRule,
    nudge: &Nudge,
    test: bool,
) -> std::result::Result<(), String> {
    match &rule.target {
        WakeTarget::Exec { exec } => {
            let mut env: HashMap<&str, String> = HashMap::new();
            env.insert("DIAVLOS_WAKE", "1".into());
            env.insert("DIAVLOS_WAKE_RULE", rule.id.clone());
            env.insert("DIAVLOS_WAKE_JSON", nudge.body());
            env.insert("DIAVLOS_ROOM", nudge.room.clone());
            env.insert("DIAVLOS_AS", rule.identity.clone());
            env.insert("DIAVLOS_TO", nudge.to.clone());
            env.insert("DIAVLOS_COUNT", nudge.count.to_string());
            env.insert("DIAVLOS_NEWEST_SEQ", nudge.newest_seq.to_string());
            env.insert("DIAVLOS_NEWEST_ID", nudge.newest_id.clone());
            if test {
                env.insert("DIAVLOS_WAKE_TEST", "1".into());
            }
            run_command(helper, exec, &env).await
        }
        WakeTarget::Url { url, secret_env } => {
            let secret = super::keys::load_wake_secret(
                &helper.paths,
                &rule.id,
                helper.config.helper.keychain,
            )
            .or_else(|| secret_env.as_ref().and_then(|v| std::env::var(v).ok()))
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "no signing secret found for this rule".to_string())?;
            webhook::post(&helper.http, url, secret.as_bytes(), nudge).await
        }
    }
}

/// Run a wake command with this helper's home, so `diavlos` in it talks
/// to this helper. Exit 0 is success.
async fn run_command(
    helper: &Helper,
    exec: &str,
    env: &HashMap<&str, String>,
) -> std::result::Result<(), String> {
    let mut cmd = tokio::process::Command::new(exec);
    cmd.envs(env)
        .env("DIAVLOS_HOME", &helper.paths.home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("could not run it: {e}"))?;
    match tokio::time::timeout(secs(EXEC_TIMEOUT), child.wait()).await {
        Ok(Ok(s)) if s.success() => Ok(()),
        Ok(Ok(s)) => Err(format!("exited with {s}")),
        Ok(Err(e)) => Err(format!("could not wait for it: {e}")),
        Err(_) => Err("still running after the time limit; stopped".into()),
    }
}

/// Deliver mode: what `watch --exec` does, run by the helper. Each message
/// goes to the command in the environment; exit 0 acks it, anything else
/// hands it back for 30 s, and too many tries quarantine it.
async fn run_deliver(helper: Arc<Helper>, rule: WakeRule) {
    let WakeTarget::Exec { exec } = &rule.target else {
        return;
    };
    let mut rx = helper.subscribe();
    let mut refused = false;
    loop {
        let Ok(room) = helper.store.room(&rule.room) else {
            wait_for(&mut rx, "", None).await;
            continue;
        };
        if no_deliver(room.class) {
            if !refused {
                helper.emit(
                    "wake_refused",
                    Some(&room.name),
                    json!({"rule": rule.id, "reason": format!("room is {}", room.class.as_str())}),
                );
                refused = true;
            }
            wait_for(&mut rx, &room.id, None).await;
            continue;
        }
        let Ok(reader) = reader_of(&helper, &room, &rule) else {
            wait_for(&mut rx, &room.id, None).await;
            continue;
        };
        if pending(&helper, &room, &reader).count == 0 {
            wait_for(&mut rx, &room.id, None).await;
            continue;
        }
        let r = match local::next(&helper, &room.name, &rule.identity, 1, None).await {
            Ok(r) => r,
            Err(_) => {
                tokio::time::sleep(secs(1)).await;
                continue;
            }
        };
        let m = &r.message;
        let (from_key, from_fingerprint) = match helper.store.member_by_name(&room.id, &m.from) {
            Ok(Some(member)) => (member.key.to_string(), member.key.fingerprint()),
            _ => (String::new(), String::new()),
        };
        let mut env: HashMap<&str, String> = HashMap::new();
        env.insert("DIAVLOS_WAKE", "1".into());
        env.insert("DIAVLOS_WAKE_RULE", rule.id.clone());
        env.insert("DIAVLOS_AS", rule.identity.clone());
        env.insert("DIAVLOS_TOKEN", r.delivery.token.clone());
        env.insert(
            "DIAVLOS_MESSAGE",
            serde_json::to_string(m).unwrap_or_default(),
        );
        env.insert("DIAVLOS_ROOM", room.name.clone());
        env.insert("DIAVLOS_ID", m.id.clone());
        env.insert("DIAVLOS_SEQ", m.seq.to_string());
        env.insert("DIAVLOS_FROM", m.from.clone());
        env.insert("DIAVLOS_TYPE", m.kind.as_str().to_string());
        env.insert("DIAVLOS_TEXT", m.text.clone());
        env.insert("DIAVLOS_FROM_KEY", from_key);
        env.insert("DIAVLOS_FROM_FINGERPRINT", from_fingerprint);
        env.insert("DIAVLOS_REPLY_TO", m.reply_to.clone().unwrap_or_default());
        env.insert("DIAVLOS_TRACE", m.trace.clone().unwrap_or_default());
        let detail = json!({"rule": rule.id, "count": 1, "target": "exec", "seq": m.seq});
        helper.emit("wake_fired", Some(&room.name), detail.clone());
        let result = run_command(&helper, exec, &env).await;
        let how = if result.is_ok() {
            SettleHow::Ack
        } else {
            SettleHow::Nack(30)
        };
        // The command may have settled it itself with DIAVLOS_TOKEN; then
        // this finds nothing to settle, which is fine.
        let _ = local::settle(&helper, &rule.identity, &r.delivery.token, how).await;
        match result {
            Ok(()) => helper.emit("wake_ok", Some(&room.name), detail),
            Err(reason) => helper.emit(
                "wake_retry",
                Some(&room.name),
                with(
                    detail,
                    json!({"reason": reason, "retry_in_ms": secs(30).as_millis() as u64}),
                ),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(count: u64, newest: u64) -> WakePending {
        WakePending {
            count,
            newest_seq: newest,
            newest_id: format!("m_{newest}"),
            looped: 0,
        }
    }

    const RENUDGE_TEST: Duration = Duration::from_secs(300);

    #[tokio::test(start_paused = true)]
    async fn a_burst_waits_for_quiet_then_fires_once() {
        let mut b = Burst::default();
        let t0 = Instant::now();
        // Messages keep landing: wait.
        assert!(matches!(
            b.decide(&p(1, 5), t0, RENUDGE_TEST),
            Next::Wait(Some(_))
        ));
        let t1 = t0 + Duration::from_millis(500);
        assert!(matches!(
            b.decide(&p(4, 8), t1, RENUDGE_TEST),
            Next::Wait(Some(_))
        ));
        // A second of quiet: fire, with everything that landed.
        let t2 = t1 + secs(QUIET);
        assert_eq!(b.decide(&p(4, 8), t2, RENUDGE_TEST), Next::Fire);
        assert!(matches!(b.fired(&p(4, 8), true, t2), Outcome::Ok));
        // Nothing new: no nudge until the re-nudge interval.
        assert_eq!(
            b.decide(&p(4, 8), t2 + secs(1), RENUDGE_TEST),
            Next::Wait(Some(t2 + RENUDGE_TEST))
        );
        assert_eq!(
            b.decide(&p(4, 8), t2 + RENUDGE_TEST, RENUDGE_TEST),
            Next::Fire
        );
    }

    #[tokio::test(start_paused = true)]
    async fn never_more_than_one_nudge_per_gap() {
        let mut b = Burst::default();
        let t0 = Instant::now();
        let _ = b.decide(&p(1, 1), t0, RENUDGE_TEST);
        let t1 = t0 + secs(QUIET);
        assert_eq!(b.decide(&p(1, 1), t1, RENUDGE_TEST), Next::Fire);
        b.fired(&p(1, 1), true, t1);
        // A new message right after: held until the gap has passed.
        let _ = b.decide(&p(2, 2), t1 + secs(1), RENUDGE_TEST);
        assert_eq!(
            b.decide(&p(2, 2), t1 + secs(3), RENUDGE_TEST),
            Next::Wait(Some(t1 + secs(MIN_GAP)))
        );
        assert_eq!(
            b.decide(&p(2, 2), t1 + secs(MIN_GAP), RENUDGE_TEST),
            Next::Fire
        );
    }

    #[tokio::test(start_paused = true)]
    async fn failures_back_off_then_give_up_until_something_new() {
        let mut b = Burst::default();
        let mut now = Instant::now();
        let _ = b.decide(&p(3, 3), now, RENUDGE_TEST);
        now += secs(QUIET);
        let mut waits = Vec::new();
        for _ in 0..BACKOFF.len() {
            assert_eq!(b.decide(&p(3, 3), now, RENUDGE_TEST), Next::Fire);
            match b.fired(&p(3, 3), false, now) {
                Outcome::Retry(w) => {
                    waits.push(w);
                    now += w;
                }
                _ => panic!("expected a retry"),
            }
        }
        assert_eq!(waits, BACKOFF.map(secs).to_vec());
        assert_eq!(b.decide(&p(3, 3), now, RENUDGE_TEST), Next::Fire);
        assert!(matches!(b.fired(&p(3, 3), false, now), Outcome::GaveUp));
        // Given up: no re-nudge for the same burst, however long it waits.
        assert_eq!(
            b.decide(&p(3, 3), now + RENUDGE_TEST * 10, RENUDGE_TEST),
            Next::Wait(None)
        );
        // A new message starts a new burst.
        let later = now + RENUDGE_TEST * 10;
        let _ = b.decide(&p(4, 4), later, RENUDGE_TEST);
        assert_eq!(
            b.decide(&p(4, 4), later + secs(QUIET), RENUDGE_TEST),
            Next::Fire
        );
    }

    #[tokio::test(start_paused = true)]
    async fn drained_means_quiet() {
        let mut b = Burst::default();
        assert_eq!(
            b.decide(&p(0, 0), Instant::now(), RENUDGE_TEST),
            Next::Wait(None)
        );
    }

    #[test]
    fn rule_file_roundtrip_has_no_secret() {
        let dir = std::env::temp_dir().join(format!("diavlos-wake-{}", rand::random::<u64>()));
        let paths = Paths { home: dir.clone() };
        let rules = vec![
            WakeRule {
                id: "w_1".into(),
                room: "ops".into(),
                identity: "runner".into(),
                target: WakeTarget::Url {
                    url: "https://hooks.example.com/runner".into(),
                    secret_env: Some("RUNNER_WAKE_SECRET".into()),
                },
                mode: WakeMode::Nudge,
                renudge_secs: None,
                created: "2026-09-27T00:00:00Z".into(),
            },
            WakeRule {
                id: "w_2".into(),
                room: "ops".into(),
                identity: "runner".into(),
                target: WakeTarget::Exec {
                    exec: "/usr/local/bin/handle".into(),
                },
                mode: WakeMode::Deliver,
                renudge_secs: Some(60),
                created: "2026-09-27T00:00:00Z".into(),
            },
        ];
        save_room(&paths, "ops", &rules).unwrap();
        let text = std::fs::read_to_string(paths.wake("ops")).unwrap();
        assert!(
            text.contains("url = \"https://hooks.example.com/runner\""),
            "{text}"
        );
        assert!(text.contains("exec = \"/usr/local/bin/handle\""), "{text}");
        assert_eq!(load_room(&paths, "ops"), rules);
        save_room(&paths, "ops", &[]).unwrap();
        assert!(!paths.wake("ops").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
