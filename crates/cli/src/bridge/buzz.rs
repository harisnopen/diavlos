//! Buzz bridge. Buzz (Block) is a team workspace on a Nostr relay, so this
//! is a Nostr client: NIP-42 AUTH, then NIP-29 group chat (kind 9) tagged
//! with the channel's `h` tag.
//!
//! Signed on both sides, and the two signatures are separate. A Diavlos
//! message is signed with the room key; the Nostr event we publish is signed
//! with the bridge's Nostr key. Neither one is turned into the other: a Buzz
//! message arrives in the room as `chat` from the bridge's own key, with the
//! Nostr author in `data`. The bridge never manufactures an approve, because
//! an approve only counts from a human key in the room.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use diavlos_client::proto::{DraftWire, Request};
use diavlos_client::{Client, Paths};
use diavlos_core::{Message, MessageType};
use futures::{SinkExt, StreamExt};
use nostr::prelude::*;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as Ws;

/// NIP-29 group chat. What a person types in a Buzz channel.
const KIND_CHAT: u16 = 9;
/// NIP-29 group metadata, relay-signed. How we list channels.
const KIND_GROUP_META: u16 = 39000;
/// NIP-42 client authentication.
const KIND_AUTH: u16 = 22242;
/// Buzz caps a frame at 64 KiB, same as our message cap.
const MAX_CONTENT: usize = 64 * 1024;

/// Read the bridge's Nostr key. `nsec1…` or 64 hex characters.
fn keys_from_env() -> Result<Keys> {
    let raw = std::env::var("BUZZ_SECRET_KEY").map_err(|_| {
        anyhow::anyhow!("set BUZZ_SECRET_KEY to the bridge's Nostr secret key (nsec1… or 64 hex)")
    })?;
    Keys::parse(raw.trim()).context("BUZZ_SECRET_KEY is not a Nostr secret key")
}

/// The relay URL as the `relay` tag must spell it. Buzz compares after
/// normalizing, but sending back exactly what we dialed is the safe move.
fn relay_tag_url(url: &str) -> String {
    url.trim().to_string()
}

/// Connect, do the NIP-42 handshake, and return the split socket.
async fn connect(
    relay: &str,
    keys: &Keys,
) -> Result<(
    futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Ws,
    >,
    futures::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
)> {
    let (ws, _) = tokio_tungstenite::connect_async(relay)
        .await
        .with_context(|| format!("connect to {relay}"))?;
    let (mut tx, mut rx) = ws.split();

    // Buzz sends the challenge without being asked, right after connect.
    let challenge = loop {
        let frame = tokio::time::timeout(Duration::from_secs(20), rx.next())
            .await
            .context("the relay sent no AUTH challenge within 20s")?
            .context("the relay closed before sending a challenge")??;
        let Ws::Text(text) = frame else { continue };
        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v[0] == "AUTH" {
            match v[1].as_str() {
                Some(c) if !c.is_empty() && c.len() <= 1024 => break c.to_string(),
                _ => bail!("the relay sent an AUTH challenge we cannot use"),
            }
        }
        if v[0] == "NOTICE" {
            eprintln!("buzz: {}", v[1].as_str().unwrap_or(""));
        }
    };

    let auth = EventBuilder::new(Kind::Custom(KIND_AUTH), "")
        .tags([
            Tag::parse(["relay", &relay_tag_url(relay)])?,
            Tag::parse(["challenge", &challenge])?,
        ])
        .finalize(keys)
        .context("sign the AUTH event")?;
    tx.send(Ws::Text(
        json!(["AUTH", serde_json::from_str::<Value>(&auth.as_json())?])
            .to_string()
            .into(),
    ))
    .await?;

    // The relay must answer every AUTH with an OK.
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(20), rx.next())
            .await
            .context("the relay did not answer our AUTH within 20s")?
            .context("the relay closed during AUTH")??;
        let Ws::Text(text) = frame else { continue };
        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v[0] == "OK" {
            if v[2] == true {
                break;
            }
            bail!(
                "the relay refused our key: {}",
                v[3].as_str().unwrap_or("no reason given")
            );
        }
        if v[0] == "NOTICE" {
            eprintln!("buzz: {}", v[1].as_str().unwrap_or(""));
        }
    }
    Ok((tx, rx))
}

/// `diavlos bridge buzz --list-channels`: what this key can see.
pub async fn list_channels(relay: &str) -> Result<()> {
    let keys = keys_from_env()?;
    let (mut tx, mut rx) = connect(relay, &keys).await?;
    tx.send(Ws::Text(
        json!(["REQ", "dv-list", {"kinds": [KIND_GROUP_META], "limit": 500}])
            .to_string()
            .into(),
    ))
    .await?;
    println!("{:<38}  name", "channel id (use with --channel)");
    let mut seen = 0usize;
    loop {
        let Some(frame) = tokio::time::timeout(Duration::from_secs(20), rx.next())
            .await
            .context("the relay went quiet while listing channels")?
        else {
            break;
        };
        let Ws::Text(text) = frame? else { continue };
        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match v[0].as_str() {
            Some("EVENT") => {
                let ev = &v[2];
                let id = ev["tags"]
                    .as_array()
                    .and_then(|tags| {
                        tags.iter()
                            .find(|t| t[0] == "d")
                            .and_then(|t| t[1].as_str())
                    })
                    .unwrap_or("");
                let name = serde_json::from_str::<Value>(ev["content"].as_str().unwrap_or("{}"))
                    .ok()
                    .and_then(|c| c["name"].as_str().map(str::to_string))
                    .unwrap_or_default();
                if !id.is_empty() {
                    println!("{id:<38}  {name}");
                    seen += 1;
                }
            }
            Some("EOSE") | Some("CLOSED") => break,
            _ => {}
        }
    }
    if seen == 0 {
        println!("(none. This key may not be a member of any channel yet.)");
    }
    Ok(())
}

/// One line for a Buzz channel: who, what type, the words.
pub fn format_for_buzz(m: &Message) -> String {
    let mut s = format!("{} ({})", m.from, m.kind);
    if let Some(to) = &m.to {
        s.push_str(&format!(" → {to}"));
    }
    s.push_str(": ");
    s.push_str(if m.tombstone {
        "(content removed)"
    } else {
        &m.text
    });
    if let Some(a) = &m.action {
        s.push_str(&format!("  [{} {}]", a.verb, a.target));
    }
    if s.len() > MAX_CONTENT {
        s.truncate(MAX_CONTENT - 3);
        s.push_str("...");
    }
    s
}

/// A kind-9 event from someone else becomes a room `chat`. Our own events
/// are skipped so nothing loops.
pub fn buzz_event_to_draft(ev: &Value, me: &str, channel: &str) -> Option<DraftWire> {
    if ev["kind"].as_u64() != Some(KIND_CHAT as u64) {
        return None;
    }
    let author = ev["pubkey"].as_str()?;
    if author == me {
        return None;
    }
    let in_channel = ev["tags"].as_array().is_some_and(|tags| {
        tags.iter()
            .any(|t| t[0] == "h" && t[1].as_str() == Some(channel))
    });
    if !in_channel {
        return None;
    }
    let text = ev["content"].as_str()?.trim().to_string();
    if text.is_empty() {
        return None;
    }
    Some(DraftWire {
        text,
        kind: Some(MessageType::Chat),
        data: json!({
            "buzz": {
                "pubkey": author,
                "event": ev["id"].as_str().unwrap_or(""),
                "created_at": ev["created_at"].as_u64().unwrap_or(0),
                "channel": channel,
            }
        }),
        ..Default::default()
    })
}

pub async fn run(
    paths: Paths,
    identity: String,
    room: String,
    relay: String,
    channel: String,
) -> Result<()> {
    let keys = keys_from_env()?;
    let me = keys.public_key().to_hex();
    let client = Client::new(paths);

    // One task owns the socket writer; everyone else sends it frames.
    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);

    // Room -> Buzz.
    let to_buzz = {
        let client = client.clone();
        let room = room.clone();
        let identity = identity.clone();
        let channel = channel.clone();
        let keys = keys.clone();
        let out_tx = out_tx.clone();
        async move {
            let mut stream = client.stream(&Request::Watch { room, identity }).await?;
            while let Some(v) = stream.next().await? {
                let m: Message = serde_json::from_value(v["message"].clone())?;
                let ev = EventBuilder::new(Kind::Custom(KIND_CHAT), format_for_buzz(&m))
                    .tags([Tag::parse(["h", &channel])?])
                    .finalize(&keys)
                    .context("sign the Buzz event")?;
                let frame = json!(["EVENT", serde_json::from_str::<Value>(&ev.as_json())?]);
                if out_tx.send(frame.to_string()).await.is_err() {
                    break;
                }
            }
            anyhow::Ok(())
        }
    };

    // Buzz -> room, with reconnect.
    let from_buzz = {
        let client = client.clone();
        let room = room.clone();
        let identity = identity.clone();
        let channel = channel.clone();
        async move {
            loop {
                let (mut tx, mut rx) = match connect(&relay, &keys).await {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("buzz: {e:#}; retrying in 5s");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                };
                // Always filter by the channel tag: a kinds-only REQ gets
                // nothing from a Buzz relay.
                let req = json!(["REQ", "dv", {"kinds": [KIND_CHAT], "#h": [channel], "limit": 0}]);
                if tx.send(Ws::Text(req.to_string().into())).await.is_err() {
                    continue;
                }
                eprintln!("buzz: connected, bridging {channel} <-> {room}");

                loop {
                    tokio::select! {
                        // Something to publish.
                        Some(frame) = out_rx.recv() => {
                            if tx.send(Ws::Text(frame.into())).await.is_err() {
                                break;
                            }
                        }
                        // Something arrived.
                        frame = rx.next() => {
                            let Some(frame) = frame else { break };
                            let Ok(Ws::Text(text)) = frame else { continue };
                            let v: Value = match serde_json::from_str(&text) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            match v[0].as_str() {
                                Some("EVENT") => {
                                    if let Some(draft) = buzz_event_to_draft(&v[2], &me, &channel) {
                                        if let Err(e) = client
                                            .call(&Request::Send {
                                                room: room.clone(),
                                                identity: identity.clone(),
                                                draft,
                                            })
                                            .await
                                        {
                                            eprintln!("room: {e}");
                                        }
                                    }
                                }
                                Some("OK") if v[2] == false => {
                                    eprintln!(
                                        "buzz: refused our event: {}",
                                        v[3].as_str().unwrap_or("no reason given")
                                    );
                                }
                                Some("CLOSED") => {
                                    eprintln!(
                                        "buzz: subscription closed: {}",
                                        v[2].as_str().unwrap_or("")
                                    );
                                    break;
                                }
                                Some("NOTICE") => {
                                    eprintln!("buzz: {}", v[1].as_str().unwrap_or(""));
                                }
                                _ => {}
                            }
                        }
                        else => break,
                    }
                }
                eprintln!("buzz: reconnecting");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            #[allow(unreachable_code)]
            anyhow::Ok(())
        }
    };

    tokio::select! {
        r = to_buzz => r,
        r = from_buzz => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: u64, pubkey: &str, channel: &str, content: &str) -> Value {
        json!({
            "id": "abc",
            "pubkey": pubkey,
            "created_at": 1_758_400_000u64,
            "kind": kind,
            "tags": [["h", channel]],
            "content": content,
            "sig": "ff"
        })
    }

    #[test]
    fn a_channel_message_becomes_a_chat() {
        let e = ev(9, "them", "uuid-1", " hello ");
        let d = buzz_event_to_draft(&e, "me", "uuid-1").unwrap();
        assert_eq!(d.text, "hello");
        assert_eq!(d.kind, Some(MessageType::Chat));
        assert_eq!(d.data["buzz"]["pubkey"], "them");
        assert_eq!(d.data["buzz"]["channel"], "uuid-1");
    }

    #[test]
    fn our_own_events_do_not_loop() {
        let mine = ev(9, "me", "uuid-1", "echo");
        assert!(buzz_event_to_draft(&mine, "me", "uuid-1").is_none());
    }

    #[test]
    fn other_channels_and_kinds_are_ignored() {
        assert!(buzz_event_to_draft(&ev(9, "them", "uuid-2", "hi"), "me", "uuid-1").is_none());
        assert!(buzz_event_to_draft(&ev(7, "them", "uuid-1", "+"), "me", "uuid-1").is_none());
        assert!(buzz_event_to_draft(&ev(9, "them", "uuid-1", "   "), "me", "uuid-1").is_none());
    }

    #[test]
    fn a_reaction_is_never_an_approve() {
        // Only kind 9 crosses, and it always crosses as chat. Nothing a
        // Buzz user can send becomes an approve in the room.
        for kind in [7u64, 46030, 46031, 40002] {
            assert!(
                buzz_event_to_draft(&ev(kind, "them", "uuid-1", "x"), "me", "uuid-1").is_none()
            );
        }
    }

    #[test]
    fn long_messages_are_cut_to_the_frame_cap() {
        let mut m = Message {
            v: 1,
            id: "m_1".into(),
            room: "r".into(),
            seq: 1,
            prev: String::new(),
            trace: None,
            from: "boss".into(),
            agent: None,
            kind: MessageType::Task,
            text: "x".repeat(MAX_CONTENT * 2),
            action: None,
            data: Value::Null,
            reply_to: None,
            to: None,
            class: Default::default(),
            ts: "2026-09-21T00:00:00Z".into(),
            action_hash: None,
            expires: None,
            once: None,
            sig: String::new(),
            content_hash: None,
            tombstone: false,
        };
        assert!(format_for_buzz(&m).len() <= MAX_CONTENT);
        m.text = "short".into();
        assert!(format_for_buzz(&m).contains("boss (task): short"));
    }
}
