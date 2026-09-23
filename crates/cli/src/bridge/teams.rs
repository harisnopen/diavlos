//! Microsoft Teams bridge, over Microsoft Graph.
//!
//! Teams has no Socket Mode: Graph's push notifications need a public HTTPS
//! endpoint, which a laptop does not have. So this signs in with the device
//! code flow (you type a code in a browser, once) and then polls the
//! channel's delta feed. No inbound firewall hole, no tunnel, no Azure
//! subscription.
//!
//! Because Graph will only let a *person* post an ordinary channel message,
//! the bridge posts as the human who signed in. That has one consequence
//! worth knowing: a message the bridge posts comes back from the delta feed
//! looking exactly like one that same person typed in Teams. We therefore
//! remember the id of everything we post and drop those on the way back, so
//! nothing loops.
//!
//! As with Slack and Buzz: a Teams message becomes a room `chat` from the
//! bridge's own key. It never becomes an approve.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use diavlos_client::proto::{DraftWire, Request};
use diavlos_client::{Client, Paths};
use diavlos_core::{Message, MessageType};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const GRAPH: &str = "https://graph.microsoft.com/v1.0";
const SCOPES: &str = "offline_access openid profile ChannelMessage.Send ChannelMessage.Read.All";
/// How often we ask the delta feed for anything new.
const POLL: Duration = Duration::from_secs(10);
/// How many of our own message ids to remember. Plenty: the feed only ever
/// hands back something we posted within a poll or two.
const OWN_IDS: usize = 500;

/// Where a channel lives. Either pasted as a Teams link or given as ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRef {
    pub tenant: Option<String>,
    pub team: String,
    pub channel: String,
}

/// Accept what "Get link to channel" puts on the clipboard, so nobody has
/// to hunt for a GUID.
///
/// `https://teams.microsoft.com/l/channel/19%3Aabc%40thread.tacv2/General?groupId=<team>&tenantId=<tenant>`
pub fn parse_channel_link(link: &str) -> Option<ChannelRef> {
    let link = link.trim();
    let rest = link.split("/l/channel/").nth(1)?;
    let (encoded, query) = match rest.split_once('?') {
        Some((path, q)) => (path.split('/').next()?, q),
        None => (rest.split('/').next()?, ""),
    };
    let channel = percent_decode(encoded);
    if !channel.starts_with("19:") {
        return None;
    }
    let mut team = None;
    let mut tenant = None;
    for pair in query.split('&') {
        match pair.split_once('=') {
            Some(("groupId", v)) => team = Some(percent_decode(v)),
            Some(("tenantId", v)) => tenant = Some(percent_decode(v)),
            _ => {}
        }
    }
    Some(ChannelRef {
        tenant,
        team: team?,
        channel,
    })
}

/// Just enough percent-decoding for a Teams link: `%3A` and `%40`.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// What we keep between runs so the human signs in once, not every time.
#[derive(Serialize, Deserialize, Default)]
struct Session {
    refresh_token: String,
    /// Where the delta feed left off. Opaque; replay the whole URL.
    delta_link: Option<String>,
}

fn session_path(paths: &Paths) -> PathBuf {
    paths.home.join("teams-session.json")
}

fn load_session(paths: &Paths) -> Session {
    std::fs::read_to_string(session_path(paths))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_session(paths: &Paths, s: &Session) -> Result<()> {
    let text = serde_json::to_string_pretty(s)?;
    // A refresh token is a credential. Same treatment as a key file.
    diavlos_core::keys::write_private(&session_path(paths), text.as_bytes())
        .context("save the Teams session")?;
    Ok(())
}

struct Auth {
    http: reqwest::Client,
    tenant: String,
    client_id: String,
    access_token: String,
    expires_at: std::time::Instant,
}

impl Auth {
    fn token_url(&self) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            self.tenant
        )
    }

    /// Sign in by showing a code the human types in a browser. No redirect
    /// URI, no local web server, no public endpoint.
    async fn device_code(
        http: reqwest::Client,
        tenant: &str,
        client_id: &str,
    ) -> Result<(Self, String)> {
        // A tenanted authority is required: /common and /consumers reject
        // the device code flow outright.
        if tenant.is_empty()
            || tenant == "common"
            || tenant == "consumers"
            || tenant == "organizations"
        {
            bail!("TEAMS_TENANT_ID must be your tenant's id or domain. Microsoft refuses the device code flow on /common, /consumers and /organizations.");
        }
        let start: Value = http
            .post(format!(
                "https://login.microsoftonline.com/{tenant}/oauth2/v2.0/devicecode"
            ))
            .form(&[("client_id", client_id), ("scope", SCOPES)])
            .send()
            .await?
            .json()
            .await?;
        let device_code = start["device_code"]
            .as_str()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "sign-in failed: {}",
                    start["error_description"]
                        .as_str()
                        .unwrap_or("no device code")
                )
            })?
            .to_string();
        // Microsoft's own wording is the clearest thing to print.
        eprintln!(
            "\n{}\n",
            start["message"]
                .as_str()
                .unwrap_or("Open the sign-in page and enter the code.")
        );
        let mut interval = start["interval"].as_u64().unwrap_or(5);

        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            let r: Value = http
                .post(format!(
                    "https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"
                ))
                .form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("client_id", client_id),
                    ("device_code", &device_code),
                ])
                .send()
                .await?
                .json()
                .await?;
            if let Some(token) = r["access_token"].as_str() {
                let refresh = r["refresh_token"].as_str().unwrap_or_default().to_string();
                if refresh.is_empty() {
                    eprintln!("teams: no refresh token came back; you will be asked to sign in again next time.");
                }
                let secs = r["expires_in"].as_u64().unwrap_or(3600);
                return Ok((
                    Auth {
                        http,
                        tenant: tenant.to_string(),
                        client_id: client_id.to_string(),
                        access_token: token.to_string(),
                        expires_at: std::time::Instant::now()
                            + Duration::from_secs(secs.saturating_sub(60)),
                    },
                    refresh,
                ));
            }
            match r["error"].as_str() {
                Some("authorization_pending") => continue,
                // Not in Microsoft's documented list, but the endpoint is
                // RFC 8628 shaped, so back off rather than fail.
                Some("slow_down") => {
                    interval += 5;
                    continue;
                }
                Some(other) => bail!(
                    "sign-in failed ({other}): {}",
                    r["error_description"].as_str().unwrap_or("")
                ),
                None => bail!("sign-in failed: no token and no error"),
            }
        }
    }

    /// Swap a saved refresh token for a fresh access token.
    async fn from_refresh(
        http: reqwest::Client,
        tenant: &str,
        client_id: &str,
        refresh_token: &str,
    ) -> Result<(Self, String)> {
        let r: Value = http
            .post(format!(
                "https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"
            ))
            .form(&[
                ("client_id", client_id),
                ("grant_type", "refresh_token"),
                ("scope", SCOPES),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await?
            .json()
            .await?;
        let token = r["access_token"].as_str().ok_or_else(|| {
            anyhow::anyhow!(
                "the saved sign-in no longer works: {}",
                r["error_description"].as_str().unwrap_or("refresh refused")
            )
        })?;
        let secs = r["expires_in"].as_u64().unwrap_or(3600);
        // Refresh tokens rotate. Keep the new one or the next start fails.
        let new_refresh = r["refresh_token"]
            .as_str()
            .unwrap_or(refresh_token)
            .to_string();
        Ok((
            Auth {
                http,
                tenant: tenant.to_string(),
                client_id: client_id.to_string(),
                access_token: token.to_string(),
                expires_at: std::time::Instant::now()
                    + Duration::from_secs(secs.saturating_sub(60)),
            },
            new_refresh,
        ))
    }

    /// The access token, refreshed if it is about to expire.
    async fn token(&mut self, refresh_token: &mut String) -> Result<&str> {
        if std::time::Instant::now() >= self.expires_at {
            let r: Value = self
                .http
                .post(self.token_url())
                .form(&[
                    ("client_id", self.client_id.as_str()),
                    ("grant_type", "refresh_token"),
                    ("scope", SCOPES),
                    ("refresh_token", refresh_token.as_str()),
                ])
                .send()
                .await?
                .json()
                .await?;
            let token = r["access_token"].as_str().ok_or_else(|| {
                anyhow::anyhow!(
                    "could not refresh the Teams sign-in: {}",
                    r["error_description"].as_str().unwrap_or("refused")
                )
            })?;
            self.access_token = token.to_string();
            let secs = r["expires_in"].as_u64().unwrap_or(3600);
            self.expires_at =
                std::time::Instant::now() + Duration::from_secs(secs.saturating_sub(60));
            if let Some(nr) = r["refresh_token"].as_str() {
                *refresh_token = nr.to_string();
            }
        }
        Ok(&self.access_token)
    }
}

/// One line for a Teams channel.
pub fn format_for_teams(m: &Message) -> String {
    let body = if m.tombstone {
        "(content removed)"
    } else {
        &m.text
    };
    let mut s = format!("<b>{}</b> ({})", esc(&m.from), m.kind);
    if let Some(to) = &m.to {
        s.push_str(&format!(" &rarr; {}", esc(to)));
    }
    s.push_str(": ");
    s.push_str(&esc(body));
    if let Some(a) = &m.action {
        s.push_str(&format!(" <i>[{} {}]</i>", esc(&a.verb), esc(&a.target)));
    }
    s
}

/// The body is HTML, so anything from the room has to be escaped. A room
/// message is untrusted text; it does not get to write markup in Teams.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Strip the HTML Teams sends back, so the room gets words, not markup.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

/// A channel message becomes a room `chat`, unless it is one of ours, a
/// system notice, a deletion, or empty.
pub fn teams_message_to_draft(m: &Value, ours: &VecDeque<String>) -> Option<DraftWire> {
    if m["messageType"].as_str().unwrap_or("message") != "message" {
        return None;
    }
    if !m["deletedDateTime"].is_null() {
        return None;
    }
    let id = m["id"].as_str().unwrap_or_default();
    if id.is_empty() || ours.iter().any(|o| o == id) {
        return None;
    }
    // A system message has no sender at all.
    let from = &m["from"];
    if from.is_null() {
        return None;
    }
    let who = from["user"]["displayName"]
        .as_str()
        .or_else(|| from["application"]["displayName"].as_str())
        .unwrap_or("someone")
        .to_string();
    let text = html_to_text(m["body"]["content"].as_str().unwrap_or(""));
    if text.is_empty() {
        return None;
    }
    Some(DraftWire {
        text: format!("{who}: {text}"),
        kind: Some(MessageType::Chat),
        data: json!({
            "teams": {
                "id": id,
                "etag": m["etag"].as_str().unwrap_or(""),
                "user": from["user"]["id"].as_str().unwrap_or(""),
                "display_name": who,
                "created": m["createdDateTime"].as_str().unwrap_or(""),
            }
        }),
        ..Default::default()
    })
}

/// `diavlos bridge teams --list-channels`: the teams and channels this
/// sign-in can see, with the ids to paste back.
pub async fn list_channels(paths: Paths) -> Result<()> {
    let (client_id, tenant) = app_from_env()?;
    let http = reqwest::Client::new();
    let mut session = load_session(&paths);
    let (mut auth, refresh) = sign_in(&http, &tenant, &client_id, &mut session).await?;
    session.refresh_token = refresh;
    save_session(&paths, &session)?;
    let mut refresh = session.refresh_token.clone();
    let token = auth.token(&mut refresh).await?.to_string();

    let teams: Value = http
        .get(format!("{GRAPH}/me/joinedTeams"))
        .bearer_auth(&token)
        .send()
        .await?
        .json()
        .await?;
    let Some(list) = teams["value"].as_array() else {
        bail!(
            "could not list your teams: {}",
            teams["error"]["message"].as_str().unwrap_or("no answer")
        );
    };
    for t in list {
        let team_id = t["id"].as_str().unwrap_or("");
        println!("\n{}  ({team_id})", t["displayName"].as_str().unwrap_or(""));
        let chans: Value = http
            .get(format!(
                "{GRAPH}/teams/{team_id}/channels?$select=id,displayName"
            ))
            .bearer_auth(&token)
            .send()
            .await?
            .json()
            .await?;
        for c in chans["value"].as_array().unwrap_or(&vec![]) {
            println!(
                "    {:<24} --team {team_id} --channel {}",
                c["displayName"].as_str().unwrap_or(""),
                c["id"].as_str().unwrap_or("")
            );
        }
    }
    println!("\nOr just paste the channel link: --link \"<Get link to channel>\"");
    Ok(())
}

fn app_from_env() -> Result<(String, String)> {
    let client_id = std::env::var("TEAMS_CLIENT_ID").map_err(|_| {
        anyhow::anyhow!("set TEAMS_CLIENT_ID to your Entra app registration's application id. The app must have \"Allow public client flows\" turned on.")
    })?;
    let tenant = std::env::var("TEAMS_TENANT_ID")
        .map_err(|_| anyhow::anyhow!("set TEAMS_TENANT_ID to your tenant id or domain"))?;
    Ok((client_id, tenant))
}

/// Use the saved sign-in if it still works, else ask for a new one.
async fn sign_in(
    http: &reqwest::Client,
    tenant: &str,
    client_id: &str,
    session: &mut Session,
) -> Result<(Auth, String)> {
    if !session.refresh_token.is_empty() {
        match Auth::from_refresh(http.clone(), tenant, client_id, &session.refresh_token).await {
            Ok(pair) => return Ok(pair),
            Err(e) => eprintln!("teams: {e:#}; signing in again"),
        }
    }
    Auth::device_code(http.clone(), tenant, client_id).await
}

pub async fn run(
    paths: Paths,
    identity: String,
    room: String,
    team: String,
    channel: String,
) -> Result<()> {
    let (client_id, tenant) = app_from_env()?;
    let http = reqwest::Client::new();
    let mut session = load_session(&paths);
    let (auth, refresh) = sign_in(&http, &tenant, &client_id, &mut session).await?;
    session.refresh_token = refresh;
    save_session(&paths, &session)?;
    eprintln!("teams: signed in, bridging {channel} <-> {room}");

    let client = Client::new(paths.clone());
    let auth = std::sync::Arc::new(tokio::sync::Mutex::new(auth));
    // Ids of messages we posted, so the delta feed does not echo them back
    // into the room. We post as a person, so this is the only thing that
    // tells our messages apart from that person's own typing.
    let ours = std::sync::Arc::new(tokio::sync::Mutex::new(VecDeque::<String>::new()));

    // Room -> Teams.
    let to_teams = {
        let client = client.clone();
        let http = http.clone();
        let auth = auth.clone();
        let ours = ours.clone();
        let paths = paths.clone();
        let (room, identity, team, channel) = (
            room.clone(),
            identity.clone(),
            team.clone(),
            channel.clone(),
        );
        async move {
            let mut stream = client
                .stream(&Request::Watch {
                    room,
                    identity: identity.clone(),
                    manual_ack: true,
                })
                .await?;
            while let Some(v) = stream.next().await? {
                let m: Message = serde_json::from_value(v["message"].clone())?;
                let mut session = load_session(&paths);
                let token = {
                    let mut a = auth.lock().await;
                    let t = a.token(&mut session.refresh_token).await?.to_string();
                    let _ = save_session(&paths, &session);
                    t
                };
                let r = http
                    .post(format!("{GRAPH}/teams/{team}/channels/{channel}/messages"))
                    .bearer_auth(&token)
                    .json(
                        &json!({"body": {"contentType": "html", "content": format_for_teams(&m)}}),
                    )
                    .send()
                    .await?;
                let body: Value = r.json().await.unwrap_or(Value::Null);
                let posted = match body["id"].as_str() {
                    Some(id) => {
                        let mut o = ours.lock().await;
                        o.push_back(id.to_string());
                        while o.len() > OWN_IDS {
                            o.pop_front();
                        }
                        true
                    }
                    None => {
                        eprintln!(
                            "teams: {}",
                            body["error"]["message"].as_str().unwrap_or("post failed")
                        );
                        false
                    }
                };
                super::settle(&client, &identity, &v, posted).await;
            }
            anyhow::Ok(())
        }
    };

    // Teams -> room, by polling the delta feed.
    let from_teams = {
        let client = client.clone();
        let http = http.clone();
        let auth = auth.clone();
        let ours = ours.clone();
        let paths = paths.clone();
        let (room, identity, team, channel) = (room, identity, team, channel);
        async move {
            let mut session = load_session(&paths);
            let mut url = session.delta_link.clone().unwrap_or_else(|| {
                format!("{GRAPH}/teams/{team}/channels/{channel}/messages/delta?$top=50")
            });
            // The first round only marks our place; it does not replay the
            // channel's history into the room.
            let mut seeding = session.delta_link.is_none();
            loop {
                let token = {
                    let mut a = auth.lock().await;
                    let t = a.token(&mut session.refresh_token).await?.to_string();
                    let _ = save_session(&paths, &session);
                    t
                };
                let r = http.get(&url).bearer_auth(&token).send().await?;
                let status = r.status();
                let body: Value = r.json().await.unwrap_or(Value::Null);
                if !status.is_success() {
                    let msg = body["error"]["message"].as_str().unwrap_or("delta failed");
                    // A stale or rejected token: start a fresh round.
                    if status.as_u16() == 400 || status.as_u16() == 410 {
                        eprintln!("teams: {msg}; starting a fresh delta round");
                        url = format!(
                            "{GRAPH}/teams/{team}/channels/{channel}/messages/delta?$top=50"
                        );
                        seeding = true;
                        session.delta_link = None;
                        let _ = save_session(&paths, &session);
                        tokio::time::sleep(POLL).await;
                        continue;
                    }
                    eprintln!("teams: {msg}");
                    tokio::time::sleep(POLL).await;
                    continue;
                }

                if !seeding {
                    for m in body["value"].as_array().unwrap_or(&vec![]) {
                        let skip = { ours.lock().await.clone() };
                        if let Some(draft) = teams_message_to_draft(m, &skip) {
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
                }

                if let Some(next) = body["@odata.nextLink"].as_str() {
                    // Still draining this round; no pause.
                    url = next.to_string();
                    continue;
                }
                if let Some(delta) = body["@odata.deltaLink"].as_str() {
                    url = delta.to_string();
                    session.delta_link = Some(url.clone());
                    let _ = save_session(&paths, &session);
                    seeding = false;
                }
                tokio::time::sleep(POLL).await;
            }
            #[allow(unreachable_code)]
            anyhow::Ok(())
        }
    };

    tokio::select! {
        r = to_teams => r,
        r = from_teams => r,
    }
}

/// Resolve `--link` or `--team/--channel` into one pair of ids.
pub fn resolve(
    link: Option<String>,
    team: Option<String>,
    channel: Option<String>,
) -> Result<(String, String)> {
    if let Some(l) = link {
        let r = parse_channel_link(&l)
            .ok_or_else(|| anyhow::anyhow!("that does not look like a Teams channel link. In Teams: channel ... > Get link to channel."))?;
        if let Some(t) = &r.tenant {
            // Saves the user hunting for it in the portal.
            if std::env::var("TEAMS_TENANT_ID").is_err() {
                eprintln!("teams: the link says tenant {t}. Set TEAMS_TENANT_ID={t}");
            }
        }
        return Ok((r.team, r.channel));
    }
    match (team, channel) {
        (Some(t), Some(c)) => Ok((t, c)),
        _ => bail!(
            "need --link, or both --team and --channel. Run with --list-channels to see them."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_channel_link_gives_the_ids() {
        let link = "https://teams.microsoft.com/l/channel/19%3A4a95f7d8db4c4e7fae857bcebe0623e6%40thread.tacv2/General?groupId=fbe2bf47-16c8-47cf-b4a5-4b9b187c508b&tenantId=2432b57b-0abd-43db-aa7b-16eadd115d34";
        let r = parse_channel_link(link).unwrap();
        assert_eq!(
            r.channel,
            "19:4a95f7d8db4c4e7fae857bcebe0623e6@thread.tacv2"
        );
        assert_eq!(r.team, "fbe2bf47-16c8-47cf-b4a5-4b9b187c508b");
        assert_eq!(
            r.tenant.as_deref(),
            Some("2432b57b-0abd-43db-aa7b-16eadd115d34")
        );
    }

    #[test]
    fn a_shared_channel_id_is_not_assumed_to_be_hex() {
        let link = "https://teams.microsoft.com/l/channel/19%3ALpxShHZZh9utjNcEmUS5aOEP9ASw85OUn05NcWYAhX81%40thread.tacv2/Shared?groupId=abc-123";
        let r = parse_channel_link(link).unwrap();
        assert_eq!(
            r.channel,
            "19:LpxShHZZh9utjNcEmUS5aOEP9ASw85OUn05NcWYAhX81@thread.tacv2"
        );
        assert_eq!(r.team, "abc-123");
        assert!(r.tenant.is_none());
    }

    #[test]
    fn rubbish_links_are_refused() {
        assert!(parse_channel_link("https://example.com").is_none());
        assert!(parse_channel_link(
            "https://teams.microsoft.com/l/channel/notachannel/x?groupId=g"
        )
        .is_none());
    }

    #[test]
    fn a_human_message_becomes_a_chat() {
        let m = json!({
            "id": "1616990032035",
            "messageType": "message",
            "deletedDateTime": null,
            "etag": "1616990032035",
            "createdDateTime": "2026-09-21T10:00:00Z",
            "from": {"application": null, "user": {"id": "u1", "displayName": "Robin"}},
            "body": {"contentType": "html", "content": "<p>deploy is green</p>"}
        });
        let d = teams_message_to_draft(&m, &VecDeque::new()).unwrap();
        assert_eq!(d.text, "Robin: deploy is green");
        assert_eq!(d.kind, Some(MessageType::Chat));
        assert_eq!(d.data["teams"]["user"], "u1");
    }

    #[test]
    fn our_own_posts_do_not_loop_back() {
        let m = json!({
            "id": "mine-1",
            "messageType": "message",
            "deletedDateTime": null,
            "from": {"user": {"id": "u1", "displayName": "Robin"}},
            "body": {"content": "<b>boss</b> (task): do it"}
        });
        let mut ours = VecDeque::new();
        ours.push_back("mine-1".to_string());
        assert!(teams_message_to_draft(&m, &ours).is_none());
        // The same text from a different id is a real message.
        let other = json!({
            "id": "theirs-1",
            "messageType": "message",
            "deletedDateTime": null,
            "from": {"user": {"id": "u2", "displayName": "Sam"}},
            "body": {"content": "hi"}
        });
        assert!(teams_message_to_draft(&other, &ours).is_some());
    }

    #[test]
    fn system_deleted_and_empty_messages_are_skipped() {
        let sys = json!({"id": "s", "messageType": "systemEventMessage", "from": null, "body": {"content": "x"}});
        assert!(teams_message_to_draft(&sys, &VecDeque::new()).is_none());
        let gone = json!({"id": "d", "messageType": "message", "deletedDateTime": "2026-01-01T00:00:00Z",
                          "from": {"user": {"displayName": "x"}}, "body": {"content": "x"}});
        assert!(teams_message_to_draft(&gone, &VecDeque::new()).is_none());
        let empty = json!({"id": "e", "messageType": "message", "deletedDateTime": null,
                           "from": {"user": {"displayName": "x"}}, "body": {"content": "<p></p>"}});
        assert!(teams_message_to_draft(&empty, &VecDeque::new()).is_none());
        let no_sender = json!({"id": "n", "messageType": "message", "deletedDateTime": null,
                               "from": null, "body": {"content": "x"}});
        assert!(teams_message_to_draft(&no_sender, &VecDeque::new()).is_none());
    }

    #[test]
    fn room_text_cannot_write_markup_into_teams() {
        let m = Message {
            v: 1,
            id: "m_1".into(),
            room: "r".into(),
            seq: 1,
            prev: String::new(),
            trace: None,
            from: "boss".into(),
            agent: None,
            kind: MessageType::Task,
            text: "<script>alert(1)</script> & done".into(),
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
        let html = format_for_teams(&m);
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&amp; done"));
    }

    #[test]
    fn html_from_teams_becomes_words() {
        assert_eq!(html_to_text("<p>hello <b>there</b></p>"), "hello there");
        assert_eq!(html_to_text("a &amp; b &lt;c&gt;"), "a & b <c>");
        assert_eq!(html_to_text("  <div> </div> "), "");
    }

    #[test]
    fn resolve_needs_a_link_or_both_ids() {
        assert!(resolve(None, Some("t".into()), None).is_err());
        assert!(resolve(None, None, None).is_err());
        let (t, c) = resolve(None, Some("t".into()), Some("c".into())).unwrap();
        assert_eq!((t.as_str(), c.as_str()), ("t", "c"));
    }
}
