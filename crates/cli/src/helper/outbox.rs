//! Member side: getting queued messages to the room's home, and what to do
//! when the home does not take one.
//!
//! Every message `send` accepted stays in the outbox until the home has it
//! in the chain, or a person drops it. A failure is sorted into one of
//! three:
//!
//! - temporary (not reached, paused, over budget, the home failed to store
//!   it): wait and try again. Never given up on for age alone.
//! - definitive (denied, invalid, not a member): `failed`, kept for a
//!   person to retry or drop.
//! - unknown (an answer this helper cannot place, such as an old home's
//!   code 1): retried, and quarantined only once the same answer has come
//!   back many times over a long time.
//!
//! Each sender has its own lane: a waiting message holds back that
//! sender's later messages, not anyone else's.

use std::sync::Arc;
use std::time::Duration;

use diavlos_core::{Error, Fate, Message, OutboxState, Room};
use tracing::{debug, warn};

use super::Helper;
use crate::net::{Link, Wire};

/// Quarantine an unknown answer once it has come back this many times...
pub const UNKNOWN_MIN_COUNT: u32 = 5;
/// ...and the first of them was at least this long ago.
pub const UNKNOWN_MIN_SECS: i64 = 3600;
/// First wait after a failure, doubled each time up to the cap.
const BACKOFF_START_SECS: u64 = 30;
const BACKOFF_MAX_SECS: u64 = 900;

/// How one submit ended.
#[derive(Debug)]
pub enum Outcome {
    /// In the chain, here and at the home.
    Sequenced(Box<Message>),
    /// Keep it; try again later. `class` is why: transport, paused, budget
    /// or home. `link_down` means this link is no good any more.
    Temporary {
        error: Error,
        class: &'static str,
        retry_after: Option<u64>,
        link_down: bool,
    },
    /// The home refused it for good.
    Definitive(Error),
    /// An answer this helper cannot place.
    Unknown(Error),
}

impl Outcome {
    /// Sort an error that happened on this side of the link: the request
    /// did not complete, so the home may or may not have the message.
    /// Retrying is safe either way: the home answers a resubmit with the
    /// copy it stored.
    fn local(error: Error) -> Outcome {
        match error {
            Error::ReachedNobody(_) | Error::TimedOut | Error::Io(_) => Outcome::Temporary {
                error,
                class: "transport",
                retry_after: None,
                link_down: true,
            },
            // Our own store failed after the home sequenced it. The next
            // try gets the stored copy back.
            Error::Db(_) | Error::Migration(_) => Outcome::Temporary {
                error,
                class: "local",
                retry_after: None,
                link_down: false,
            },
            other => Outcome::Unknown(other),
        }
    }

    /// Sort the home's answer.
    pub fn from_reply(
        code: i32,
        msg: &str,
        fate: Option<Fate>,
        retry_after: Option<u64>,
    ) -> Outcome {
        let error = Error::from_code(code, msg);
        let temporary = |class: &'static str| Outcome::Temporary {
            error: Error::from_code(code, msg),
            class,
            retry_after,
            link_down: false,
        };
        match (fate, code) {
            (_, diavlos_core::error::CODE_ROOM_PAUSED) => temporary("paused"),
            (_, diavlos_core::error::CODE_REACHED_NOBODY | diavlos_core::error::CODE_TIMED_OUT) => {
                temporary("transport")
            }
            (Some(Fate::Temporary), _) if retry_after.is_some() => temporary("budget"),
            (Some(Fate::Temporary), _) => temporary("home"),
            (Some(Fate::Definitive), _) => Outcome::Definitive(error),
            // An older home says no fate: go by the code alone.
            (
                None,
                diavlos_core::error::CODE_NOT_IN_ROOM
                | diavlos_core::error::CODE_NAME_TAKEN
                | diavlos_core::error::CODE_DENIED,
            ) => Outcome::Definitive(error),
            (None, _) => Outcome::Unknown(error),
        }
    }
}

/// Submit one message to the home and store the sequenced copy. Storing it
/// takes it out of the outbox in the same transaction.
pub async fn submit(helper: &Helper, room: &Room, link: &Arc<dyn Link>, msg: &Message) -> Outcome {
    let req = Wire::Submit {
        room_id: room.id.clone(),
        message: msg.clone(),
    };
    let reply = match link.request(&req).await {
        Ok(r) => r,
        Err(e) => return Outcome::local(e),
    };
    match reply {
        Wire::Sequenced { message } => {
            if helper.faults.hit("outbox.after_sequenced") {
                return Outcome::local(Error::Io(std::io::Error::other(
                    "injected fault after the home sequenced it",
                )));
            }
            match helper.apply_from_home(room, std::slice::from_ref(&message)) {
                Ok(_) => Outcome::Sequenced(Box::new(message)),
                Err(Error::Invalid(_)) => {
                    // We are behind. Catch up; the sync brings this one too.
                    match super::peers::sync_from_home(helper, room, link).await {
                        Ok(()) => Outcome::Sequenced(Box::new(message)),
                        Err(e) => Outcome::local(e),
                    }
                }
                Err(e) => Outcome::local(e),
            }
        }
        Wire::Err {
            code,
            msg,
            fate,
            retry_after,
        } => Outcome::from_reply(code, &msg, fate, retry_after),
        other => Outcome::Unknown(Error::Invalid(format!("unexpected {}", other.label()))),
    }
}

/// How long to wait after the `attempts`-th failure: 30 s doubling to
/// 15 min, give or take a fifth so many helpers do not retry in step.
fn backoff(attempts: u32) -> Duration {
    let base = BACKOFF_START_SECS
        .saturating_mul(1u64 << attempts.min(10))
        .min(BACKOFF_MAX_SECS);
    let jitter = rand::random_range(0..=base / 5);
    Duration::from_secs(base - base / 10 + jitter)
}

/// Send what is due in a room, lane by lane. Returns `Err` only when the
/// link is no good any more, so the caller reconnects.
pub async fn flush_outbox(
    helper: &Helper,
    room: &Room,
    link: &Arc<dyn Link>,
) -> diavlos_core::Result<()> {
    let lock = helper.submit_lock(&room.id).await;
    let _guard = lock.lock().await;
    for (sender, lane) in helper.store.outbox_lanes(&room.id)? {
        for entry in lane {
            let now = helper.now();
            let now_s = helper.now_ts();
            if entry.state == OutboxState::Waiting {
                if let Some(at) = entry.retry_at.as_deref() {
                    if at > now_s.as_str() {
                        // The head of this lane is not due: the whole lane waits.
                        break;
                    }
                }
            }
            let Some(msg) = entry.message else {
                // Sealed with a key this helper no longer has. Nothing can
                // send it; say so rather than retry forever.
                helper.store.outbox_unknown(
                    &entry.msg_id,
                    "cannot decrypt this queued message with this helper's key",
                    None,
                    &now_s,
                    &now_s,
                    1,
                    0,
                )?;
                helper.emit(
                    "send_quarantined",
                    Some(&room.id),
                    serde_json::json!({"id": entry.msg_id, "sender": sender, "reason": "unreadable"}),
                );
                continue;
            };
            match submit(helper, room, link, &msg).await {
                Outcome::Sequenced(m) => {
                    debug!(room = %room.id, id = %m.id, seq = m.seq, "queued message delivered");
                }
                Outcome::Temporary {
                    error,
                    class,
                    retry_after,
                    link_down,
                } => {
                    let wait = match retry_after {
                        Some(secs) => Duration::from_secs(secs.max(1)),
                        None => backoff(entry.attempts),
                    };
                    let at = (now + chrono::Duration::from_std(wait).unwrap_or_default())
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                    helper.store.outbox_wait(
                        &msg.id,
                        class,
                        &error.to_string(),
                        Some(error.code()),
                        &at,
                        &now_s,
                    )?;
                    debug!(room = %room.id, id = %msg.id, class, retry_at = %at, "queued message waits");
                    if link_down {
                        return Err(error);
                    }
                    if class == "paused" {
                        // Every lane would hear the same.
                        return Ok(());
                    }
                    break;
                }
                Outcome::Definitive(error) => {
                    warn!(room = %room.id, id = %msg.id, error = %error, "queued message refused by home; kept as failed");
                    helper.store.outbox_fail(
                        &msg.id,
                        &error.to_string(),
                        Some(error.code()),
                        &now_s,
                    )?;
                    helper.emit(
                        "send_failed",
                        Some(&room.id),
                        serde_json::json!({"id": msg.id, "sender": sender, "code": error.code(), "reason": error.to_string()}),
                    );
                }
                Outcome::Unknown(error) => {
                    let at = (now
                        + chrono::Duration::from_std(backoff(entry.attempts)).unwrap_or_default())
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                    let state = helper.store.outbox_unknown(
                        &msg.id,
                        &error.to_string(),
                        Some(error.code()),
                        &at,
                        &now_s,
                        UNKNOWN_MIN_COUNT,
                        UNKNOWN_MIN_SECS,
                    )?;
                    if state == OutboxState::Quarantined {
                        warn!(room = %room.id, id = %msg.id, error = %error, "queued message quarantined");
                        helper.emit(
                            "send_quarantined",
                            Some(&room.id),
                            serde_json::json!({"id": msg.id, "sender": sender, "code": error.code(), "reason": error.to_string()}),
                        );
                        continue;
                    }
                    break;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_are_sorted_by_fate_then_code() {
        let t = |o: Outcome| match o {
            Outcome::Temporary { class, .. } => format!("temporary/{class}"),
            Outcome::Definitive(_) => "definitive".into(),
            Outcome::Unknown(_) => "unknown".into(),
            Outcome::Sequenced(_) => "sequenced".into(),
        };
        assert_eq!(
            t(Outcome::from_reply(7, "paused", None, None)),
            "temporary/paused"
        );
        assert_eq!(
            t(Outcome::from_reply(
                1,
                "over",
                Some(Fate::Temporary),
                Some(60)
            )),
            "temporary/budget"
        );
        assert_eq!(
            t(Outcome::from_reply(1, "disk", Some(Fate::Temporary), None)),
            "temporary/home"
        );
        assert_eq!(
            t(Outcome::from_reply(
                1,
                "invalid",
                Some(Fate::Definitive),
                None
            )),
            "definitive"
        );
        assert_eq!(
            t(Outcome::from_reply(6, "denied", None, None)),
            "definitive"
        );
        // An old home's code 1 is not understood: kept and retried.
        assert_eq!(
            t(Outcome::from_reply(1, "over budget", None, None)),
            "unknown"
        );
    }

    #[test]
    fn a_broken_link_mid_frame_is_temporary() {
        let e = Error::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "early eof",
        ));
        assert!(matches!(
            Outcome::local(e),
            Outcome::Temporary {
                class: "transport",
                link_down: true,
                ..
            }
        ));
    }

    #[test]
    fn backoff_grows_and_stops_at_the_cap() {
        let first = backoff(0).as_secs();
        assert!((27..=33).contains(&first), "{first}");
        let late = backoff(20).as_secs();
        assert!((810..=990).contains(&late), "{late}");
    }

    use crate::helper::testkit::{
        connect, draft, draft_as, helper, room_with, Fault, ScriptedLink, TempHome,
    };
    use diavlos_core::MessageType;

    fn io(what: &str) -> Error {
        Error::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            what.to_string(),
        ))
    }

    /// Every message accepted into the outbox is in exactly one place: in
    /// the chain, or still in the outbox (in any state).
    fn accounted(h: &Helper, ids: &[&str]) {
        for id in ids {
            let in_chain = h.store.message_by_id(id).unwrap().is_some();
            let queued = h.store.outbox_get(id).unwrap().is_some();
            assert!(
                in_chain ^ queued,
                "{id}: in chain {in_chain}, in outbox {queued}"
            );
        }
    }

    struct Pair {
        _dirs: (TempHome, TempHome),
        home: Arc<Helper>,
        member: Arc<Helper>,
        room: Room,
    }

    async fn pair() -> Pair {
        let dirs = (TempHome::new("home"), TempHome::new("member"));
        let home = helper("home", &dirs.0, None);
        let member = helper("member", &dirs.1, None);
        let r = room_with(
            &home,
            "ops",
            &[
                (&member, "bot", "bot", false),
                (&member, "bot2", "bot2", false),
            ],
        )
        .await;
        let room = member.store.room_by_id(&r.id).unwrap().unwrap();
        Pair {
            _dirs: dirs,
            home,
            member,
            room,
        }
    }

    fn state(h: &Helper, id: &str) -> Option<OutboxState> {
        h.store.outbox_get(id).unwrap().map(|e| e.state)
    }

    #[tokio::test]
    async fn a_write_that_breaks_mid_frame_keeps_the_message_and_it_arrives_once() {
        let p = pair().await;
        let m = draft(&p.member, "ops", "bot", "hello").await;
        p.member.store.outbox_add(&m).unwrap();

        let link = ScriptedLink::new(connect(&p.home, &p.member).await, "submit");
        link.then(Fault::FailBefore(io("broken pipe mid-frame")));
        let link: Arc<dyn Link> = link;
        assert!(
            flush_outbox(&p.member, &p.room, &link).await.is_err(),
            "the link is done"
        );
        assert_eq!(state(&p.member, &m.id), Some(OutboxState::Waiting));
        assert!(p.home.store.message_by_id(&m.id).unwrap().is_none());

        // A new link makes transport waits due at once.
        let link = connect(&p.home, &p.member).await;
        p.member.store.outbox_wake(&p.room.id, "transport").unwrap();
        flush_outbox(&p.member, &p.room, &link).await.unwrap();
        assert_eq!(state(&p.member, &m.id), None);
        let at_home = p.home.store.message_by_id(&m.id).unwrap().unwrap();
        let here = p.member.store.message_by_id(&m.id).unwrap().unwrap();
        assert_eq!(at_home.seq, here.seq);
        accounted(&p.member, &[&m.id]);
    }

    #[tokio::test]
    async fn a_reply_lost_mid_frame_is_recovered_without_a_duplicate() {
        let p = pair().await;
        let m = draft(&p.member, "ops", "bot", "hello").await;
        p.member.store.outbox_add(&m).unwrap();
        let before = p.home.store.message_count(&p.room.id).unwrap();

        let link = ScriptedLink::new(connect(&p.home, &p.member).await, "submit");
        link.then(Fault::FailAfter(io("connection reset reading the reply")));
        let link: Arc<dyn Link> = link;
        assert!(flush_outbox(&p.member, &p.room, &link).await.is_err());
        // The home has it; this side does not know yet, and keeps it.
        assert!(p.home.store.message_by_id(&m.id).unwrap().is_some());
        assert_eq!(state(&p.member, &m.id), Some(OutboxState::Waiting));

        let link = connect(&p.home, &p.member).await;
        p.member.store.outbox_wake(&p.room.id, "transport").unwrap();
        flush_outbox(&p.member, &p.room, &link).await.unwrap();
        assert_eq!(state(&p.member, &m.id), None);
        assert_eq!(p.home.store.message_count(&p.room.id).unwrap(), before + 1);
        accounted(&p.member, &[&m.id]);
    }

    #[tokio::test]
    async fn a_restart_between_sequencing_and_storing_it_here_loses_nothing() {
        let p = pair().await;
        let m = draft(&p.member, "ops", "bot", "hello").await;
        p.member.store.outbox_add(&m).unwrap();
        p.member.faults.arm("outbox.after_sequenced");
        let link = connect(&p.home, &p.member).await;
        assert!(flush_outbox(&p.member, &p.room, &link).await.is_err());
        assert!(p.home.store.message_by_id(&m.id).unwrap().is_some());

        // Restart: a new helper on the same files.
        let again = helper("member", &p._dirs.1, None);
        assert_eq!(state(&again, &m.id), Some(OutboxState::Waiting));
        let link = connect(&p.home, &again).await;
        again.store.outbox_wake(&p.room.id, "transport").unwrap();
        flush_outbox(&again, &p.room, &link).await.unwrap();
        assert_eq!(state(&again, &m.id), None);
        accounted(&again, &[&m.id]);
    }

    #[tokio::test]
    async fn a_refusal_is_kept_as_failed_and_the_lane_moves_on() {
        let p = pair().await;
        // A claim on something that is not a task: the home says no for good.
        let chat = draft(&p.member, "ops", "bot", "not a task").await;
        let bad = draft_as(
            &p.member,
            "ops",
            "bot",
            "mine",
            MessageType::Claim,
            Some(&chat.id),
        )
        .await;
        let after = draft(&p.member, "ops", "bot", "after the bad one").await;
        let other = draft(&p.member, "ops", "bot2", "other lane").await;
        for m in [&chat, &bad, &after, &other] {
            p.member.store.outbox_add(m).unwrap();
        }
        let mut events = p.member.subscribe_events();
        let link = connect(&p.home, &p.member).await;
        flush_outbox(&p.member, &p.room, &link).await.unwrap();

        assert_eq!(state(&p.member, &bad.id), Some(OutboxState::Failed));
        let e = p.member.store.outbox_get(&bad.id).unwrap().unwrap();
        assert!(e.reason.unwrap().contains("not a task"));
        for m in [&chat, &after, &other] {
            assert_eq!(state(&p.member, &m.id), None, "{} was held back", m.text);
        }
        let mut kinds = Vec::new();
        while let Ok(ev) = events.try_recv() {
            kinds.push(ev.kind);
        }
        assert_eq!(
            kinds.iter().filter(|k| *k == "send_failed").count(),
            1,
            "{kinds:?}"
        );
        accounted(&p.member, &[&chat.id, &bad.id, &after.id, &other.id]);
    }

    #[tokio::test]
    async fn an_old_homes_unknown_answer_is_retried_then_quarantined_never_dropped() {
        let p = pair().await;
        let m = draft(&p.member, "ops", "bot", "hello").await;
        p.member.store.outbox_add(&m).unwrap();
        let link = ScriptedLink::new(connect(&p.home, &p.member).await, "submit");
        for _ in 0..5 {
            link.then(Fault::Answer(Box::new(Wire::err(
                1,
                "something this helper does not know",
            ))));
        }
        let link: Arc<dyn Link> = link;
        for round in 0..5 {
            flush_outbox(&p.member, &p.room, &link).await.unwrap();
            let want = if round < 4 {
                OutboxState::Waiting
            } else {
                OutboxState::Quarantined
            };
            assert_eq!(state(&p.member, &m.id), Some(want), "round {round}");
            // Past any backoff.
            p.member.skew_clock(20 * 60);
        }
        assert!(p.home.store.message_by_id(&m.id).unwrap().is_none());
        accounted(&p.member, &[&m.id]);

        // Retry sends the very same message: same id, same signature.
        let sig = m.sig.clone();
        assert!(p
            .member
            .store
            .outbox_retry(&m.id, &p.member.now_ts())
            .unwrap());
        flush_outbox(&p.member, &p.room, &link).await.unwrap();
        let at_home = p.home.store.message_by_id(&m.id).unwrap().unwrap();
        assert_eq!(at_home.sig, sig);
        accounted(&p.member, &[&m.id]);
    }

    #[tokio::test]
    async fn a_budget_spent_for_hours_holds_one_lane_and_drops_nothing() {
        let p = pair().await;
        let first = draft(&p.member, "ops", "bot", "one").await;
        let second = draft(&p.member, "ops", "bot", "two").await;
        let other = draft(&p.member, "ops", "bot2", "other lane").await;
        for m in [&first, &second, &other] {
            p.member.store.outbox_add(m).unwrap();
        }
        let five_hours = 5 * 3600;
        let link = ScriptedLink::new(connect(&p.home, &p.member).await, "submit");
        link.then(Fault::Answer(Box::new(Wire::Err {
            code: 1,
            msg: "over budget: room has used its daily budget of 1000 messages".into(),
            fate: Some(Fate::Temporary),
            retry_after: Some(five_hours),
        })));
        let link: Arc<dyn Link> = link;
        flush_outbox(&p.member, &p.room, &link).await.unwrap();
        let e = p.member.store.outbox_get(&first.id).unwrap().unwrap();
        assert_eq!(e.state, OutboxState::Waiting);
        assert_eq!(e.reason_class.as_deref(), Some("budget"));
        // Its lane waits behind it; the other lane does not.
        assert_eq!(state(&p.member, &second.id), Some(OutboxState::Pending));
        assert_eq!(state(&p.member, &other.id), None);

        // Hours pass, many flushes: nothing is given up on.
        for _ in 0..4 {
            p.member.skew_clock(3600);
            flush_outbox(&p.member, &p.room, &link).await.unwrap();
            if p.member.now_ts() < e.retry_at.clone().unwrap() {
                assert_eq!(state(&p.member, &first.id), Some(OutboxState::Waiting));
            }
        }
        p.member.skew_clock(3600);
        flush_outbox(&p.member, &p.room, &link).await.unwrap();
        assert_eq!(state(&p.member, &first.id), None);
        assert_eq!(state(&p.member, &second.id), None);
        let a = p.home.store.message_by_id(&first.id).unwrap().unwrap();
        let b = p.home.store.message_by_id(&second.id).unwrap().unwrap();
        assert!(a.seq < b.seq, "a lane keeps its order");
        accounted(&p.member, &[&first.id, &second.id, &other.id]);
    }

    #[tokio::test]
    async fn a_paused_room_holds_messages_until_resume() {
        let p = pair().await;
        crate::helper::local::control_for_test(
            &p.home,
            "ops",
            "default",
            diavlos_core::ControlOp::Pause,
        )
        .await;
        let m = draft(&p.member, "ops", "bot", "hello").await;
        p.member.store.outbox_add(&m).unwrap();
        let link = connect(&p.home, &p.member).await;
        flush_outbox(&p.member, &p.room, &link).await.unwrap();
        let e = p.member.store.outbox_get(&m.id).unwrap().unwrap();
        assert_eq!(e.state, OutboxState::Waiting);
        assert_eq!(e.reason_class.as_deref(), Some("paused"));

        crate::helper::local::control_for_test(
            &p.home,
            "ops",
            "default",
            diavlos_core::ControlOp::Resume,
        )
        .await;
        // The member hears of the resume, which makes the message due.
        crate::helper::peers::sync_from_home(&p.member, &p.room, &link)
            .await
            .unwrap();
        flush_outbox(&p.member, &p.room, &link).await.unwrap();
        assert_eq!(state(&p.member, &m.id), None);
        accounted(&p.member, &[&m.id]);
    }
}
