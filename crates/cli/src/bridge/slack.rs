//! Slack bridge over Socket Mode: no public URL needed, works from a
//! laptop. Room messages go to one channel; channel messages come into
//! the room as `chat` from the bridge's own key, with the Slack user in
//! `data`. The bridge never pretends a Slack message is a signed approve.

use diavlos_client::proto::{DraftWire, Request};
use diavlos_client::{Client, Paths};
use diavlos_core::{Message, MessageType};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message as Ws;

pub async fn run(
    paths: Paths,
    identity: String,
    room: String,
    channel: String,
) -> anyhow::Result<()> {
    let bot = std::env::var("SLACK_BOT_TOKEN")
        .map_err(|_| anyhow::anyhow!("set SLACK_BOT_TOKEN (xoxb-...)"))?;
    let app = std::env::var("SLACK_APP_TOKEN")
        .map_err(|_| anyhow::anyhow!("set SLACK_APP_TOKEN (xapp-...)"))?;
    let http = reqwest::Client::new();
    let client = Client::new(paths.clone());

    // Room -> Slack.
    let to_slack = {
        let client = client.clone();
        let http = http.clone();
        let bot = bot.clone();
        let channel = channel.clone();
        let room = room.clone();
        let identity = identity.clone();
        async move {
            let mut stream = client.stream(&Request::Watch { room, identity }).await?;
            while let Some(v) = stream.next().await? {
                let m: Message = serde_json::from_value(v["message"].clone())?;
                let text = format_for_slack(&m);
                let r = http
                    .post("https://slack.com/api/chat.postMessage")
                    .bearer_auth(&bot)
                    .json(&json!({"channel": channel, "text": text}))
                    .send()
                    .await?;
                let body: Value = r.json().await.unwrap_or(Value::Null);
                if body["ok"].as_bool() != Some(true) {
                    eprintln!("slack: {}", body["error"].as_str().unwrap_or("post failed"));
                }
            }
            anyhow::Ok(())
        }
    };

    // Slack -> room.
    let from_slack = {
        let client = client.clone();
        let http = http.clone();
        let channel = channel.clone();
        let room = room.clone();
        let identity = identity.clone();
        async move {
            loop {
                let open: Value = http
                    .post("https://slack.com/api/apps.connections.open")
                    .bearer_auth(&app)
                    .send()
                    .await?
                    .json()
                    .await?;
                let Some(url) = open["url"].as_str() else {
                    anyhow::bail!(
                        "slack: apps.connections.open failed: {}",
                        open["error"].as_str().unwrap_or("?")
                    );
                };
                let (ws, _) = tokio_tungstenite::connect_async(url).await?;
                let (mut tx, mut rx) = ws.split();
                eprintln!("slack: connected, bridging #{channel} <-> {room}");
                while let Some(frame) = rx.next().await {
                    let Ok(Ws::Text(text)) = frame else { continue };
                    let env: Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if let Some(id) = env["envelope_id"].as_str() {
                        let _ = tx
                            .send(Ws::Text(json!({"envelope_id": id}).to_string().into()))
                            .await;
                    }
                    if env["type"] == "disconnect" {
                        break;
                    }
                    if let Some(draft) = slack_event_to_draft(&env, &channel) {
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
                eprintln!("slack: reconnecting");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            #[allow(unreachable_code)]
            anyhow::Ok(())
        }
    };

    tokio::select! {
        r = to_slack => r,
        r = from_slack => r,
    }
}

/// One line for humans: who, what type, the words. Everything that came
/// from the room is escaped, so a member cannot ping `<!channel>`, mention
/// someone with `<@U…>`, or dress a link up as another with `<url|text>`.
pub fn format_for_slack(m: &Message) -> String {
    let mut s = format!("*{}* ({})", escape(&m.from), m.kind);
    if let Some(to) = &m.to {
        s.push_str(&format!(" → {}", escape(to)));
    }
    s.push_str(": ");
    if m.tombstone {
        s.push_str("(content removed)");
    } else {
        s.push_str(&escape(&m.text));
    }
    if let Some(a) = &m.action {
        s.push_str(&format!("  [{} {}]", escape(&a.verb), escape(&a.target)));
    }
    s
}

/// Slack's three control characters, as its docs say to escape them.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A channel message from a person becomes a room `chat`. Bot messages
/// (including our own) are ignored so nothing loops.
pub fn slack_event_to_draft(envelope: &Value, channel: &str) -> Option<DraftWire> {
    if envelope["type"] != "events_api" {
        return None;
    }
    let ev = &envelope["payload"]["event"];
    if ev["type"] != "message" || ev["channel"] != channel {
        return None;
    }
    if ev.get("bot_id").is_some() || ev.get("subtype").is_some() {
        return None;
    }
    let text = ev["text"].as_str()?.trim().to_string();
    if text.is_empty() {
        return None;
    }
    Some(DraftWire {
        text,
        kind: Some(MessageType::Chat),
        data: json!({
            "slack": {
                "user": ev["user"].as_str().unwrap_or(""),
                "ts": ev["ts"].as_str().unwrap_or(""),
                "channel": channel,
            }
        }),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_person_messages_and_skips_bots() {
        let env = json!({
            "type": "events_api",
            "envelope_id": "x",
            "payload": {"event": {"type": "message", "channel": "C1", "user": "U1", "text": " hi ", "ts": "1.2"}}
        });
        let d = slack_event_to_draft(&env, "C1").unwrap();
        assert_eq!(d.text, "hi");
        assert_eq!(d.kind, Some(MessageType::Chat));
        assert_eq!(d.data["slack"]["user"], "U1");
        let bot = json!({
            "type": "events_api",
            "payload": {"event": {"type": "message", "channel": "C1", "bot_id": "B1", "text": "echo"}}
        });
        assert!(slack_event_to_draft(&bot, "C1").is_none());
        assert!(slack_event_to_draft(&env, "C2").is_none());
    }

    #[test]
    fn room_text_cannot_ping_or_disguise_links() {
        let m = Message {
            v: 1,
            id: "m_1".into(),
            room: "r".into(),
            seq: 1,
            prev: String::new(),
            trace: None,
            from: "bob".into(),
            agent: None,
            kind: MessageType::Chat,
            text: "<!channel> see <https://evil.example|https://good.example> & <@U1>".into(),
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
        let out = format_for_slack(&m);
        assert!(!out.contains('<') && !out.contains('>'), "{out}");
        assert!(out.contains("&lt;!channel&gt;"), "{out}");
        assert!(out.contains("&amp; &lt;@U1&gt;"), "{out}");
    }
}
