//! `diavlos`: one binary. Helper, CLI, MCP server, web UI, bridge.

mod bridge;
mod config;
mod doctor;
mod helper;
mod hook;
mod install;
mod mcp;
mod net;
mod service;
mod web;

use std::io::Read;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use diavlos_client::proto::{
    AskResult, CheckApproveResult, DeliveryItem, DraftWire, ExportResult, InviteResult, JoinResult,
    NextResult, OutboxItem, ReadResult, Request, RotateResult, SendResult, StatusResult, WhoEntry,
};
use diavlos_client::{Client, Paths};
use diavlos_core::{ControlOp, DataClass, Error, Message, MessageType, Role};

#[derive(Parser)]
#[command(name = "diavlos", version, about = "The channel between AI agents.")]
struct Cli {
    /// Which local key to act as. Made on first use. `default` is you.
    #[arg(
        long = "as",
        global = true,
        env = "DIAVLOS_AS",
        default_value = "default"
    )]
    identity: String,
    /// Data directory. Default ~/.diavlos
    #[arg(long, global = true, env = "DIAVLOS_HOME")]
    home: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Make a room. You are the owner.
    New {
        room: String,
        #[arg(long, default_value = "")]
        about: String,
        /// Keep messages this many days, then drop their content. Default: keep forever.
        #[arg(long)]
        retention: Option<u32>,
        /// Data class of the room: public, internal, confidential, pii.
        #[arg(long)]
        class: Option<DataClass>,
    },
    /// Make one signed invite for one new member. Prints a note to paste
    /// into that agent's session.
    Invite {
        room: String,
        name: String,
        /// Mark the key as a person who can approve.
        #[arg(long)]
        human: bool,
        /// Pin the invite to one machine (its node id, from `diavlos status`).
        #[arg(long = "for", value_name = "NODE-ID")]
        for_node: Option<String>,
        /// observer, chat, task-giver or approver. Default task-giver, or
        /// approver with --human.
        #[arg(long)]
        role: Option<Role>,
    },
    /// Join with an invite. Starts the helper if needed.
    Join {
        invite: String,
    },
    /// Give a member a role. Observer, chat, task-giver, approver. Can expire.
    Grant {
        room: String,
        name: String,
        #[arg(long)]
        role: Role,
        /// Expiry date (YYYY-MM-DD) or RFC 3339 time.
        #[arg(long)]
        until: Option<String>,
    },
    /// New room key. Everyone out. Re-invite who you keep.
    Rotate {
        room: String,
    },
    /// Send a message. Reads from stdin if no text.
    Send {
        room: String,
        text: Option<String>,
        /// chat, task, question, reply, done, claim, release, approve, deny
        #[arg(long = "type", default_value = "chat")]
        kind: MessageType,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        reply_to: Option<String>,
        #[arg(long)]
        trace: Option<String>,
        /// Any JSON, passed as-is in `data`.
        #[arg(long)]
        data: Option<String>,
        /// public, internal, confidential, pii
        #[arg(long)]
        class: Option<DataClass>,
        #[arg(long)]
        json: bool,
    },
    /// Send a question and wait for a reply to that exact message. Exit
    /// code 4 if it times out, 6 if the answer is a deny.
    Ask {
        room: String,
        text: String,
        #[arg(long, default_value_t = 120)]
        timeout: u64,
        /// Structured intent as JSON: {"verb":"deploy","target":"api","params":{}}.
        /// A human then approves the exact action, not the words.
        #[arg(long)]
        action: Option<String>,
        #[arg(long)]
        trace: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Wait for the next message from someone else. Skips your own and
    /// helper notices. Acks it once printed: that is receipt, not "your
    /// script finished". For that, use --manual-ack and `diavlos ack`.
    Next {
        room: String,
        /// Give up after this many seconds (exit code 4). Default: wait.
        #[arg(long)]
        timeout: Option<u64>,
        /// Do not ack. Prints a token; the message stays yours until you
        /// `diavlos ack` it, `nack` it, or the lease runs out, and is then
        /// handed out again.
        #[arg(long)]
        manual_ack: bool,
        /// How long it stays yours, in seconds. Default 600.
        #[arg(long)]
        lease: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    /// Look at messages from your bookmark onward. Never deletes and
    /// moves nothing, unless --ack.
    Read {
        room: String,
        #[arg(long)]
        since: Option<u64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Settle what this prints as taken on.
        #[arg(long)]
        ack: bool,
        #[arg(long)]
        json: bool,
    },
    /// Settle a message from `next --manual-ack`: taken on. Not "finished":
    /// say `done` in the room for that.
    Ack {
        token: String,
    },
    /// Still working on it: keep it yours longer.
    Renew {
        token: String,
        /// Seconds from now. Default 600.
        #[arg(long)]
        lease: Option<u64>,
    },
    /// Not now: hand it out again later.
    Nack {
        token: String,
        /// Seconds until it is handed out again.
        #[arg(long = "retry-in", default_value_t = 60)]
        retry_in: u64,
    },
    /// Messages handed out and not simply done: leased, delayed, and
    /// quarantined after being handed out too many times.
    Deliveries {
        room: String,
        /// Every key on this helper, not only --as.
        #[arg(long)]
        all: bool,
        /// Bring this quarantined message (seq) back to be handed out again.
        #[arg(long, value_name = "SEQ")]
        replay: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    /// Stream messages. Run a script for each one. The script gets the
    /// message in DIAVLOS_MESSAGE (JSON) and the sender's key in
    /// DIAVLOS_FROM_KEY, never on the command line.
    Watch {
        room: String,
        #[arg(long)]
        exec: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Take a task. Two claims on one task: first wins, second is told no.
    Claim {
        room: String,
        task_id: String,
    },
    /// Give a task back.
    Release {
        room: String,
        task_id: String,
    },
    /// Who is here: kind, role, key fingerprint, what they said they do,
    /// when last seen.
    Who {
        room: String,
        #[arg(long)]
        json: bool,
    },
    /// Say no to an ask. Logged like a yes.
    Deny {
        room: String,
        msg_id: String,
        #[arg(long)]
        reason: String,
    },
    /// Exit 0 if a valid, unexpired, unused human approve exists for
    /// exactly this action and the room's home records its spend. For
    /// deploy scripts to call before they act. Exit 6: no; exit 3: the
    /// home is out of reach, nothing spent, try again.
    CheckApprove {
        room: String,
        action_json: String,
        /// Your id for this operation, the same on every retry of it: a
        /// retry then gets the recorded answer, not a refusal. Deduplicate
        /// on it where you act.
        #[arg(long = "op", value_name = "ID")]
        op_id: Option<String>,
    },
    /// Kill switch. Nothing moves until resume.
    Pause {
        room: String,
    },
    Resume {
        room: String,
    },
    /// Silence one member (--off to unmute).
    Mute {
        room: String,
        name: String,
        #[arg(long)]
        off: bool,
    },
    /// Cut one member's key for good.
    Revoke {
        room: String,
        name: String,
    },
    /// Edit the room's rule file. One rule for now: which verbs need a
    /// human approve.
    Policy {
        room: String,
        /// Print the file instead of opening $EDITOR.
        #[arg(long)]
        show: bool,
    },
    /// Signed audit bundle to stdout.
    Export {
        room: String,
        /// Only messages from this date (YYYY-MM-DD) or time on.
        #[arg(long)]
        since: Option<String>,
    },
    /// Check a bundle. Works with no helper running. For auditors.
    Verify {
        bundle: PathBuf,
        /// The owner key fingerprint you expect (from `diavlos who`). A
        /// bundle signed by any other owner key fails.
        #[arg(long)]
        owner: Option<String>,
    },
    /// Legal hold. Retention stops deleting.
    Hold {
        room: String,
        #[arg(long, conflicts_with = "off")]
        on: bool,
        #[arg(long)]
        off: bool,
    },
    /// JSONL stream of everything the helper does. Feed it to Splunk.
    Events {
        #[arg(long)]
        follow: bool,
    },
    /// Checks config, network, keys, disk. Paste the output in a ticket.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Start the helper with your login (systemd, launchd, Windows logon).
    Service {
        #[command(subcommand)]
        action: ServiceCmd,
    },
    /// Messages still to reach a room's home, and ones it would not take.
    /// Nothing leaves the outbox except by reaching the home or by `drop`.
    Outbox {
        #[command(subcommand)]
        which: Option<OutboxCmd>,
    },
    /// See rooms and peers.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Stop the helper.
    Stop,
    /// Wake-up hooks: install one, or run it.
    Hook {
        #[command(subcommand)]
        which: HookCmd,
    },
    /// Start the MCP server (stdio), or write an agent tool's MCP config
    /// so it has the room tools.
    Mcp {
        #[command(subcommand)]
        which: Option<McpCmd>,
    },
    /// Browser UI on localhost. Prints a one-time login link.
    Web {
        #[arg(long, default_value_t = 7777)]
        port: u16,
    },
    /// Bridge a room to a chat service.
    Bridge {
        #[command(subcommand)]
        which: BridgeCmd,
    },
    /// Run the helper in the foreground. Commands start it for you;
    /// this is for services and debugging.
    Helper {
        /// Windows only: run under the service control manager.
        #[arg(long, hide = true)]
        service: bool,
    },
}

#[derive(Subcommand)]
enum HookCmd {
    /// Write the wake-up hook for an agent tool, so room messages land in
    /// its turn instead of it polling.
    Install {
        /// Which tool. Run `diavlos hook install --list` to see them.
        #[arg(long = "for", value_name = "TOOL")]
        target: Option<String>,
        /// Which room to watch.
        #[arg(long)]
        room: Option<String>,
        /// Write the project-scoped file instead of the user-wide one.
        #[arg(long)]
        project: bool,
        /// Print what would change and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// List the tools and what each one can do.
        #[arg(long)]
        list: bool,
    },
    /// What the hook itself runs. Reads the hook payload on stdin and
    /// answers on stdout. You do not call this by hand.
    Run {
        #[arg(long)]
        room: String,
        /// Answer in the shape a session-start hook wants.
        #[arg(long)]
        session_start: bool,
    },
}

#[derive(Subcommand)]
enum McpCmd {
    /// Write the MCP config for an agent tool. Config writing, not an
    /// adapter: the tool already speaks MCP, we just add one server entry.
    Install {
        /// Which tool, or `all`. Run `diavlos mcp install --list` to see them.
        #[arg(long = "for", value_name = "TOOL")]
        target: Option<String>,
        /// Write the project-scoped file instead of the user-wide one.
        #[arg(long)]
        project: bool,
        /// Print what would change and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// List the tools and where each one's config lives.
        #[arg(long)]
        list: bool,
    },
}

#[derive(Subcommand)]
enum OutboxCmd {
    /// Every queued message, its state and why. The default.
    List {
        /// Only this room.
        room: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Put a failed or quarantined message back in line, unchanged: same
    /// id, same signature.
    Retry { id: String },
    /// Give up on a failed or quarantined message. Its content is wiped;
    /// a record that it was dropped stays.
    Drop { id: String },
}

#[derive(Subcommand)]
enum ServiceCmd {
    Install,
    Uninstall,
}

#[derive(Subcommand)]
enum BridgeCmd {
    /// Slack, via Socket Mode. Needs SLACK_BOT_TOKEN and SLACK_APP_TOKEN.
    Slack {
        #[arg(long)]
        room: String,
        /// Slack channel id, like C0123456789.
        #[arg(long)]
        channel: String,
    },
    /// Microsoft Teams, over Graph. Signs in with a device code, then
    /// polls. Needs TEAMS_CLIENT_ID and TEAMS_TENANT_ID.
    Teams {
        #[arg(long)]
        room: Option<String>,
        /// Paste "Get link to channel" from Teams instead of the two ids.
        #[arg(long)]
        link: Option<String>,
        /// The team id (a guid).
        #[arg(long)]
        team: Option<String>,
        /// The channel id, like 19:...@thread.tacv2.
        #[arg(long)]
        channel: Option<String>,
        /// Print the teams and channels this sign-in can see, then stop.
        #[arg(long)]
        list_channels: bool,
    },
    /// Buzz (Block), over its Nostr relay. Needs BUZZ_SECRET_KEY.
    Buzz {
        #[arg(long)]
        room: Option<String>,
        /// The relay, like wss://buzz.example.com.
        #[arg(long)]
        relay: String,
        /// The Buzz channel id (a uuid). Use --list-channels to find it.
        #[arg(long)]
        channel: Option<String>,
        /// Print the channels this key can see, then stop.
        #[arg(long)]
        list_channels: bool,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let paths = match Paths::resolve(cli.home.clone()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let code = match run(cli, paths).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.code()
        }
    };
    std::process::exit(code);
}

fn other(e: anyhow::Error) -> Error {
    Error::Other(format!("{e:#}"))
}

fn until_to_ts(until: &str) -> String {
    if until.len() == 10 {
        format!("{until}T23:59:59Z")
    } else {
        until.to_string()
    }
}

async fn run(cli: Cli, paths: Paths) -> Result<i32, Error> {
    let identity = cli.identity.clone();
    let client = Client::new(paths.clone());
    let control = |room: String, control: ControlOp| Request::Control {
        room,
        identity: identity.clone(),
        control,
    };
    match cli.cmd {
        Cmd::Helper { service } => {
            #[cfg(windows)]
            if service {
                return service::run_as_service(paths).map(|_| 0).map_err(other);
            }
            let _ = service;
            helper::run(paths).await.map(|_| 0).map_err(other)
        }
        Cmd::Hook { which } => match which {
            HookCmd::Install {
                target,
                room,
                project,
                dry_run,
                list,
            } => hook_install(&identity, target, room, project, dry_run, list).map_err(other),
            HookCmd::Run {
                room,
                session_start,
            } => hook_run(&client, &identity, &room, session_start).await,
        },
        Cmd::Mcp { which } => match which {
            None => mcp::serve(paths, identity).await.map(|_| 0).map_err(other),
            Some(McpCmd::Install {
                target,
                project,
                dry_run,
                list,
            }) => mcp_install(&paths, &identity, target, project, dry_run, list).map_err(other),
        },
        Cmd::Web { port } => web::serve(paths, identity, port)
            .await
            .map(|_| 0)
            .map_err(other),
        Cmd::Bridge { which } => match which {
            BridgeCmd::Slack { room, channel } => {
                bridge::slack::run(paths, identity, room, channel)
                    .await
                    .map(|_| 0)
                    .map_err(other)
            }
            BridgeCmd::Teams {
                room,
                link,
                team,
                channel,
                list_channels,
            } => {
                if list_channels {
                    return bridge::teams::list_channels(paths)
                        .await
                        .map(|_| 0)
                        .map_err(other);
                }
                let Some(room) = room else {
                    return Err(other(anyhow::anyhow!("need --room")));
                };
                let (team, channel) = bridge::teams::resolve(link, team, channel).map_err(other)?;
                bridge::teams::run(paths, identity, room, team, channel)
                    .await
                    .map(|_| 0)
                    .map_err(other)
            }
            BridgeCmd::Buzz {
                room,
                relay,
                channel,
                list_channels,
            } => {
                if list_channels {
                    return bridge::buzz::list_channels(&relay)
                        .await
                        .map(|_| 0)
                        .map_err(other);
                }
                let (Some(room), Some(channel)) = (room, channel) else {
                    return Err(other(anyhow::anyhow!(
                        "need --room and --channel. Run with --list-channels to find the channel id."
                    )));
                };
                bridge::buzz::run(paths, identity, room, relay, channel)
                    .await
                    .map(|_| 0)
                    .map_err(other)
            }
        },
        Cmd::New {
            room,
            about,
            retention,
            class,
        } => {
            let v = client
                .call(&Request::NewRoom {
                    name: room.clone(),
                    about,
                    identity,
                    retention_days: retention,
                    class,
                })
                .await?;
            let r: diavlos_core::Room = serde_json::from_value(v)?;
            println!("made room {} ({}). You are the owner.", r.name, r.id);
            println!("Invite someone: diavlos invite {} <name> [--human]", r.name);
            Ok(0)
        }
        Cmd::Invite {
            room,
            name,
            human,
            for_node,
            role,
        } => {
            let v = client
                .call(&Request::Invite {
                    room: room.clone(),
                    name: name.clone(),
                    human,
                    for_node,
                    role,
                    identity,
                })
                .await?;
            let r: InviteResult = serde_json::from_value(v)?;
            println!("Paste this into {}'s session:\n", r.name);
            println!("    diavlos join {}\n", r.invite);
            print!(
                "This invite is for \"{}\" ({}), works once, and expires {}.",
                r.name, r.role, r.expires
            );
            if let Some(n) = r.for_node {
                print!(" It only works on node {}.", short(&n));
            }
            println!();
            Ok(0)
        }
        Cmd::Join { invite } => {
            let v = client.call(&Request::Join { invite, identity }).await?;
            let r: JoinResult = serde_json::from_value(v)?;
            println!(
                "joined {} as {}. {} members, {} messages so far.",
                r.room.name,
                r.name,
                r.members.len(),
                r.messages
            );
            Ok(0)
        }
        Cmd::Grant {
            room,
            name,
            role,
            until,
        } => {
            let v = client
                .call(&control(
                    room,
                    ControlOp::Grant {
                        name,
                        role,
                        until: until.as_deref().map(until_to_ts),
                    },
                ))
                .await?;
            let r: SendResult = serde_json::from_value(v)?;
            println!("{}", r.message.text);
            Ok(0)
        }
        Cmd::Rotate { room } => {
            let v = client.call(&Request::Rotate { room, identity }).await?;
            let r: RotateResult = serde_json::from_value(v)?;
            println!(
                "rotated. Old room kept for audit as {}. New room {} ({}) has only you in it.",
                r.old_room_name, r.room.name, r.room.id
            );
            println!(
                "Re-invite who you keep: diavlos invite {} <name>",
                r.room.name
            );
            Ok(0)
        }
        Cmd::Send {
            room,
            text,
            kind,
            to,
            reply_to,
            trace,
            data,
            class,
            json,
        } => {
            let text = match text {
                Some(t) => t,
                None => {
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s.trim_end().to_string()
                }
            };
            let data = match data {
                Some(d) => serde_json::from_str(&d)
                    .map_err(|e| Error::Invalid(format!("--data is not JSON: {e}")))?,
                None => serde_json::Value::Null,
            };
            let req = match (kind, &reply_to) {
                (MessageType::Approve, Some(id)) => Request::Approve {
                    room,
                    identity,
                    msg_id: id.clone(),
                },
                (MessageType::Deny, Some(id)) => Request::Deny {
                    room,
                    identity,
                    msg_id: id.clone(),
                    reason: text,
                },
                _ => Request::Send {
                    room,
                    identity,
                    draft: DraftWire {
                        text,
                        kind: Some(kind),
                        to,
                        reply_to,
                        trace,
                        data,
                        class,
                        action: None,
                    },
                },
            };
            let v = client.call(&req).await?;
            let r: SendResult = serde_json::from_value(v)?;
            if json {
                println!("{}", serde_json::to_string(&r.message)?);
            } else if r.delivered {
                println!("sent {} (seq {})", r.message.id, r.message.seq);
            } else {
                println!(
                    "queued {} (the room's home is offline; it will go when it is back)",
                    r.message.id
                );
            }
            Ok(0)
        }
        Cmd::Ask {
            room,
            text,
            timeout,
            action,
            trace,
            json,
        } => {
            let action = match action {
                Some(a) => Some(
                    serde_json::from_str::<diavlos_core::Action>(&a)
                        .map_err(|e| Error::Invalid(format!("--action is not valid: {e}")))?,
                ),
                None => None,
            };
            let v = client
                .call(&Request::Ask {
                    room,
                    identity,
                    draft: DraftWire {
                        text,
                        kind: Some(MessageType::Question),
                        trace,
                        action,
                        ..Default::default()
                    },
                    timeout_secs: timeout,
                })
                .await?;
            let r: AskResult = serde_json::from_value(v)?;
            print_message(&r.reply, json)?;
            Ok(if r.reply.kind == MessageType::Deny {
                6
            } else {
                0
            })
        }
        Cmd::Next {
            room,
            timeout,
            manual_ack,
            lease,
            json,
        } => {
            let v = client
                .call(&Request::Next {
                    room,
                    identity: identity.clone(),
                    timeout_secs: timeout.unwrap_or(0),
                    manual_ack: true,
                    lease_secs: lease,
                })
                .await?;
            let r: NextResult = serde_json::from_value(v)?;
            if manual_ack {
                if json {
                    println!("{}", serde_json::to_string(&r)?);
                } else {
                    print_message(&r.message, false)?;
                    println!(
                        "  token {} (yours until {}, attempt {}): diavlos ack {}",
                        r.delivery.token,
                        r.delivery.lease_until,
                        r.delivery.attempt,
                        r.delivery.token
                    );
                }
                return Ok(0);
            }
            print_message(&r.message, json)?;
            // Printed: that is receipt. If the ack does not land, the
            // message is handed out again once the lease runs out.
            if let Err(e) = client
                .call(&Request::Ack {
                    identity,
                    token: r.delivery.token,
                })
                .await
            {
                eprintln!("warning: could not ack it ({e}); it will be handed out again");
            }
            Ok(0)
        }
        Cmd::Read {
            room,
            since,
            limit,
            ack,
            json,
        } => {
            let v = client
                .call(&Request::Read {
                    room,
                    identity,
                    since,
                    limit,
                    ack,
                })
                .await?;
            let r: ReadResult = serde_json::from_value(v)?;
            for m in &r.messages {
                print_message(m, json)?;
            }
            Ok(0)
        }
        Cmd::Ack { token } => {
            client.call(&Request::Ack { identity, token }).await?;
            println!("acked");
            Ok(0)
        }
        Cmd::Renew { token, lease } => {
            let v = client
                .call(&Request::Renew {
                    identity,
                    token,
                    lease_secs: lease,
                })
                .await?;
            println!(
                "yours until {}",
                v["lease_until"].as_str().unwrap_or("later")
            );
            Ok(0)
        }
        Cmd::Nack { token, retry_in } => {
            let v = client
                .call(&Request::Nack {
                    identity,
                    token,
                    retry_in_secs: Some(retry_in),
                })
                .await?;
            match v["state"].as_str() {
                Some("quarantined") => println!(
                    "handed back, and quarantined: it has been handed out too many times. \
                     `diavlos deliveries <room>` lists it"
                ),
                _ => println!(
                    "handed back; it comes round again at {}",
                    v["retry_at"].as_str().unwrap_or("?")
                ),
            }
            Ok(0)
        }
        Cmd::Deliveries {
            room,
            all,
            replay,
            json,
        } => {
            if let Some(seq) = replay {
                client
                    .call(&Request::Replay {
                        room,
                        identity,
                        seq,
                    })
                    .await?;
                println!("message {seq} will be handed out again next");
                return Ok(0);
            }
            let v = client
                .call(&Request::Deliveries {
                    room,
                    identity,
                    all,
                })
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(0);
            }
            let items: Vec<DeliveryItem> = serde_json::from_value(v)?;
            if items.is_empty() {
                println!("nothing handed out and waiting");
            }
            for d in items {
                let when = match d.state.as_str() {
                    "leased" => format!(" until {}", d.lease_until.unwrap_or_default()),
                    "delayed" => format!(" until {}", d.retry_at.unwrap_or_default()),
                    _ => String::new(),
                };
                println!(
                    "[{}] {} ({}) to {}: {}{}, handed out {} time{}\n    {}",
                    d.seq,
                    d.from,
                    d.kind.map(|k| k.as_str()).unwrap_or("?"),
                    d.reader,
                    d.state,
                    when,
                    d.attempt,
                    if d.attempt == 1 { "" } else { "s" },
                    d.text
                );
            }
            Ok(0)
        }
        Cmd::Watch { room, exec, json } => {
            let mut stream = client
                .stream(&Request::Watch {
                    room: room.clone(),
                    identity: identity.clone(),
                    manual_ack: true,
                })
                .await?;
            while let Some(v) = stream.next().await? {
                let m: Message = serde_json::from_value(v["message"].clone())?;
                let token = v["delivery"]["token"].as_str().unwrap_or("").to_string();
                print_message(&m, json)?;
                let mut handled = true;
                if let Some(script) = &exec {
                    let status = std::process::Command::new(script)
                        .env("DIAVLOS_TOKEN", &token)
                        .env("DIAVLOS_MESSAGE", serde_json::to_string(&m)?)
                        .env("DIAVLOS_ROOM", &room)
                        .env("DIAVLOS_ID", &m.id)
                        .env("DIAVLOS_SEQ", m.seq.to_string())
                        .env("DIAVLOS_FROM", &m.from)
                        .env("DIAVLOS_TYPE", m.kind.as_str())
                        .env("DIAVLOS_TEXT", &m.text)
                        .env("DIAVLOS_FROM_KEY", v["from_key"].as_str().unwrap_or(""))
                        .env(
                            "DIAVLOS_FROM_FINGERPRINT",
                            v["from_fingerprint"].as_str().unwrap_or(""),
                        )
                        .env("DIAVLOS_REPLY_TO", m.reply_to.clone().unwrap_or_default())
                        .env("DIAVLOS_TRACE", m.trace.clone().unwrap_or_default())
                        .status();
                    handled = match status {
                        Ok(s) if s.success() => true,
                        Ok(s) => {
                            eprintln!("script exited with {s} for {}; handing it back", m.id);
                            false
                        }
                        Err(e) => {
                            eprintln!("could not run {script}: {e}; handing it back");
                            false
                        }
                    };
                }
                // Ack once handled; a failed script hands it back, and after
                // too many tries it is quarantined, never dropped.
                let settle = if handled {
                    Request::Ack {
                        identity: identity.clone(),
                        token,
                    }
                } else {
                    Request::Nack {
                        identity: identity.clone(),
                        token,
                        retry_in_secs: Some(30),
                    }
                };
                client.call(&settle).await?;
            }
            Ok(0)
        }
        Cmd::Claim { room, task_id } => {
            let v = client
                .call(&Request::Claim {
                    room,
                    identity,
                    task_id: task_id.clone(),
                })
                .await?;
            let r: SendResult = serde_json::from_value(v)?;
            println!("claimed {task_id} ({})", r.message.id);
            Ok(0)
        }
        Cmd::Release { room, task_id } => {
            let v = client
                .call(&Request::Release {
                    room,
                    identity,
                    task_id: task_id.clone(),
                })
                .await?;
            let r: SendResult = serde_json::from_value(v)?;
            println!("released {task_id} ({})", r.message.id);
            Ok(0)
        }
        Cmd::Who { room, json } => {
            let v = client.call(&Request::Who { room }).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(0);
            }
            let who: Vec<WhoEntry> = serde_json::from_value(v)?;
            println!(
                "name             kind    role        key               state    last seen            says"
            );
            for w in who {
                let mut flags = Vec::new();
                if w.owner {
                    flags.push("owner");
                }
                if w.revoked {
                    flags.push("revoked");
                } else if w.muted {
                    flags.push("muted");
                } else if w.expired {
                    flags.push("expired");
                }
                let state = if w.revoked {
                    "gone"
                } else if w.online {
                    "online"
                } else {
                    "away"
                };
                let says = {
                    let p = &w.profile;
                    let parts: Vec<String> = ["vendor", "model", "owner"]
                        .iter()
                        .filter_map(|k| {
                            p.get(k)
                                .and_then(|v| v.as_str())
                                .map(|s| format!("{k}={s}"))
                        })
                        .collect();
                    parts.join(" ")
                };
                println!(
                    "{:<16} {:<7} {:<11} {:<17} {:<8} {:<20} {}{}",
                    w.name,
                    w.kind,
                    w.role,
                    w.fingerprint,
                    state,
                    w.last_seen.unwrap_or_default(),
                    says,
                    if flags.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", flags.join(","))
                    },
                );
            }
            Ok(0)
        }
        Cmd::Deny {
            room,
            msg_id,
            reason,
        } => {
            let v = client
                .call(&Request::Deny {
                    room,
                    identity,
                    msg_id,
                    reason,
                })
                .await?;
            let r: SendResult = serde_json::from_value(v)?;
            println!("denied ({})", r.message.id);
            Ok(0)
        }
        Cmd::CheckApprove {
            room,
            action_json,
            op_id,
        } => {
            let action: diavlos_core::Action = serde_json::from_str(&action_json)
                .map_err(|e| Error::Invalid(format!("action is not valid: {e}")))?;
            let v = client
                .call(&Request::CheckApprove {
                    room,
                    action,
                    identity,
                    op_id,
                })
                .await?;
            let r: CheckApproveResult = serde_json::from_value(v)?;
            println!(
                "approved by {} ({}), valid until {}. Spent for operation {} (audit seq {}).",
                r.approved_by, r.approve_id, r.expires, r.op_id, r.audit_seq
            );
            Ok(0)
        }
        Cmd::Pause { room } => {
            client.call(&control(room, ControlOp::Pause)).await?;
            println!("paused");
            Ok(0)
        }
        Cmd::Resume { room } => {
            client.call(&control(room, ControlOp::Resume)).await?;
            println!("resumed");
            Ok(0)
        }
        Cmd::Mute { room, name, off } => {
            let op = if off {
                ControlOp::Unmute { name }
            } else {
                ControlOp::Mute { name }
            };
            let v = client.call(&control(room, op)).await?;
            let r: SendResult = serde_json::from_value(v)?;
            println!("{}", r.message.text);
            Ok(0)
        }
        Cmd::Revoke { room, name } => {
            let v = client
                .call(&control(room, ControlOp::Revoke { name }))
                .await?;
            let r: SendResult = serde_json::from_value(v)?;
            println!("{}", r.message.text);
            Ok(0)
        }
        Cmd::Hold { room, on, off } => {
            let on = on || !off;
            client.call(&control(room, ControlOp::Hold { on })).await?;
            println!("legal hold {}", if on { "on" } else { "off" });
            Ok(0)
        }
        Cmd::Policy { room, show } => {
            let path = paths.policy(&room);
            if !path.exists() {
                diavlos_core::Policy::default()
                    .save(&path)
                    .map_err(|e| Error::Other(e.to_string()))?;
            }
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .ok();
            match (show, editor) {
                (false, Some(ed)) => {
                    let status = std::process::Command::new(ed).arg(&path).status()?;
                    if !status.success() {
                        return Err(Error::Other("editor failed".into()));
                    }
                    diavlos_core::Policy::load(&path)?;
                    println!("policy saved: {}", path.display());
                }
                _ => {
                    println!("# {}", path.display());
                    print!("{}", std::fs::read_to_string(&path)?);
                }
            }
            Ok(0)
        }
        Cmd::Export { room, since } => {
            let v = client
                .call(&Request::Export {
                    room,
                    identity,
                    since,
                })
                .await?;
            let r: ExportResult = serde_json::from_value(v)?;
            print!("{}", r.bundle);
            eprintln!("exported {} messages", r.count);
            Ok(0)
        }
        Cmd::Verify { bundle, owner } => {
            let text = std::fs::read_to_string(&bundle)?;
            let mut report = diavlos_core::bundle::verify(&text)?;
            if let Some(want) = owner {
                if !want.eq_ignore_ascii_case(&report.owner_fingerprint) {
                    report.problems.push(format!(
                        "owner key is {}, not the {} you expected",
                        report.owner_fingerprint, want
                    ));
                }
            }
            println!(
                "room {} ({}), exported by {} at {}, {} messages (seq {}..{}), {} tombstones{}",
                report.room,
                report.room_id,
                report.exported_by,
                report.exported_at,
                report.count,
                report.first_seq,
                report.last_seq,
                report.tombstones,
                if report.chain_from_genesis {
                    ", chain from genesis"
                } else {
                    ""
                }
            );
            println!("owner key {}", report.owner_fingerprint);
            if report.ok() {
                println!("OK: every signature verifies and the chain is whole.");
                println!(
                    "That proves the bundle is whole, not whose it is: check the owner key \
matches `diavlos who`, or pass --owner."
                );
                Ok(0)
            } else {
                for p in &report.problems {
                    println!("PROBLEM: {p}");
                }
                Ok(1)
            }
        }
        Cmd::Events { follow } => {
            let mut stream = client.stream(&Request::Events { follow }).await?;
            while let Some(v) = stream.next().await? {
                println!("{}", serde_json::to_string(&v)?);
            }
            Ok(0)
        }
        Cmd::Doctor { json } => {
            let report = doctor::run(&paths).await;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                doctor::print(&report);
            }
            Ok(if report["ok"].as_bool().unwrap_or(false) {
                0
            } else {
                1
            })
        }
        Cmd::Service { action } => {
            let lines = match action {
                ServiceCmd::Install => service::install(&paths).map_err(other)?,
                ServiceCmd::Uninstall => service::uninstall(&paths).map_err(other)?,
            };
            for l in lines {
                println!("{l}");
            }
            Ok(0)
        }
        Cmd::Outbox { which } => match which.unwrap_or(OutboxCmd::List {
            room: None,
            json: false,
        }) {
            OutboxCmd::List { room, json } => {
                let v = client.call(&Request::Outbox { room }).await?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&v)?);
                    return Ok(0);
                }
                let items: Vec<OutboxItem> = serde_json::from_value(v)?;
                if items.is_empty() {
                    println!("the outbox is empty: everything sent has reached its room");
                }
                for i in items {
                    let mut line = format!(
                        "{}  {}  {}  {:<11} {} attempt{}",
                        i.id,
                        i.room,
                        i.sender,
                        i.state,
                        i.attempts,
                        if i.attempts == 1 { "" } else { "s" }
                    );
                    if let Some(at) = &i.retry_at {
                        line.push_str(&format!(", next try {at}"));
                    }
                    if let Some(r) = &i.reason {
                        line.push_str(&format!("\n    {r}"));
                    }
                    if !i.text.is_empty() {
                        line.push_str(&format!("\n    {}", i.text));
                    }
                    println!("{line}");
                }
                Ok(0)
            }
            OutboxCmd::Retry { id } => {
                client
                    .call(&Request::OutboxRetry { id: id.clone() })
                    .await?;
                println!("{id} is back in line; it goes as soon as the home takes it");
                Ok(0)
            }
            OutboxCmd::Drop { id } => {
                client.call(&Request::OutboxDrop { id: id.clone() }).await?;
                println!("dropped {id}; its content is gone, a record stays");
                Ok(0)
            }
        },
        Cmd::Status { json } => {
            let v = client.call(&Request::Status).await?;
            let s: StatusResult = serde_json::from_value(v)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&s)?);
                return Ok(0);
            }
            println!(
                "diavlos {}  node {}  home {}",
                s.version, s.node, s.home_dir
            );
            if s.rooms.is_empty() {
                println!("no rooms yet. Try: diavlos new <room>");
            }
            for r in &s.rooms {
                let mut outbox = String::new();
                for (n, what) in [
                    (r.queued, "queued"),
                    (r.failed, "failed"),
                    (r.quarantined, "quarantined"),
                    (r.quarantined_deliveries, "undeliverable"),
                ] {
                    if n > 0 {
                        outbox.push_str(&format!(", {n} {what}"));
                    }
                }
                println!(
                    "  {:<20} {:<7} {:<9} {} members, {} messages{}{} (you: {})",
                    r.name,
                    if r.home { "home" } else { "member" },
                    if r.connected { "linked" } else { "offline" },
                    r.members,
                    r.messages,
                    outbox,
                    if r.paused { ", paused" } else { "" },
                    r.me.join(", "),
                );
                if r.failed + r.quarantined > 0 {
                    println!("  {:<20} see: diavlos outbox {}", "", r.name);
                }
                if r.quarantined_deliveries > 0 {
                    println!("  {:<20} see: diavlos deliveries {} --all", "", r.name);
                }
            }
            Ok(0)
        }
        Cmd::Stop => {
            match client.call_if_running(&Request::Stop).await? {
                Some(_) => {
                    // Wait until it is really gone, so `stop` followed by
                    // another command does not race the old process.
                    for _ in 0..50 {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        if !client.is_running().await {
                            break;
                        }
                    }
                    println!("helper stopped");
                }
                None => println!("helper is not running"),
            }
            Ok(0)
        }
    }
}

/// `diavlos hook install`.
fn hook_install(
    identity: &str,
    target: Option<String>,
    room: Option<String>,
    project: bool,
    dry_run: bool,
    list: bool,
) -> anyhow::Result<i32> {
    if list {
        println!("Diavlos can install a wake-up hook for:\n");
        for t in hook::TARGETS {
            println!("  {:<14} {}", t.id, t.label);
            println!("  {:<14} {}", "", t.note);
        }
        println!("\nUse: diavlos hook install --for <tool> --room <room>");
        return Ok(0);
    }
    let Some(target) = target else {
        anyhow::bail!(
            "say which tool: --for <{}>, or --list to see them",
            hook::target_ids()
        );
    };
    let Some(room) = room else {
        anyhow::bail!("say which room to watch: --room <room>");
    };
    let Some(t) = hook::target(&target) else {
        anyhow::bail!("unknown tool {target}. Try one of: {}", hook::target_ids());
    };
    let o = hook::install(t, &room, identity, project, dry_run)?;
    let what = if dry_run {
        if o.changed {
            "would write"
        } else {
            "already set"
        }
    } else if o.changed {
        "wrote"
    } else {
        "already set"
    };
    println!("{:<14} {what} {}", o.tool, o.path.display());
    if o.changed {
        println!("{:<14} {}", "", t.note);
    }
    Ok(0)
}

/// `diavlos hook run`: what the hook itself calls. Answers on stdout in the
/// shape the tool expects, and never blocks: if the helper is not up or the
/// room is quiet, it says nothing and the agent carries on as normal.
async fn hook_run(
    client: &Client,
    identity: &str,
    room: &str,
    session_start: bool,
) -> Result<i32, Error> {
    let payload = hook::read_stdin();
    let stop_hook_active = payload
        .get("stop_hook_active")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // A turn our own block started: we will not block again, so leave the
    // messages unread and let the next stop pick them up.
    if stop_hook_active {
        return Ok(0);
    }

    // Look at what is owed without taking it, and without waiting. Showing
    // a message is not the agent accepting it: it stays owed, and this
    // reminder comes back, until the agent takes it with diavlos_next and
    // acks it. A hook must never hold up a turn, so any failure here is
    // silence, not an error. Own messages and housekeeping are never owed.
    let v = match client
        .call(&Request::Peek {
            room: room.to_string(),
            identity: identity.to_string(),
            limit: 20,
        })
        .await
    {
        Ok(v) => v,
        Err(_) => return Ok(0),
    };
    let Ok(owed) = serde_json::from_value::<Vec<Message>>(v) else {
        return Ok(0);
    };
    if let Some(out) = hook::decide(&owed, stop_hook_active, session_start) {
        println!("{out}");
    }
    Ok(0)
}

/// `diavlos mcp install`. Prints what it did, one line per file, so the
/// user can see which files were touched without opening them.
fn mcp_install(
    paths: &Paths,
    identity: &str,
    target: Option<String>,
    project: bool,
    dry_run: bool,
    list: bool,
) -> anyhow::Result<i32> {
    if list {
        println!("Diavlos can write the MCP config for:\n");
        for t in install::TOOLS {
            println!("  {:<14} {}", t.id, t.label);
            if let Some(n) = t.note {
                println!("  {:<14} {n}", "");
            }
        }
        println!("\nUse: diavlos mcp install --for <tool>   (or --for all)");
        return Ok(0);
    }
    let Some(target) = target else {
        anyhow::bail!(
            "say which tool: --for <{}|all>, or --list to see them",
            install::tool_ids()
        );
    };

    // Pass DIAVLOS_HOME through only when the user set one, so the entry
    // does not freeze a path they may move.
    let explicit_home = std::env::var_os("DIAVLOS_HOME")
        .map(|_| paths.home.clone())
        .or_else(|| {
            // --home was given on this invocation.
            let default = directories::BaseDirs::new().map(|b| b.home_dir().join(".diavlos"));
            match default {
                Some(d) if d != paths.home => Some(paths.home.clone()),
                _ => None,
            }
        });

    let targets: Vec<&'static install::Tool> = if target == "all" {
        install::TOOLS.iter().collect()
    } else {
        match install::tool(&target) {
            Some(t) => vec![t],
            None => anyhow::bail!(
                "unknown tool {target}. Try one of: {}, all",
                install::tool_ids()
            ),
        }
    };

    // One key per file: tools that share a file act as the same key. A tool
    // with no file for this scope fails on its own; the rest still install.
    let mut failed = 0;
    let mut located = Vec::new();
    for t in &targets {
        match install::target_path(t, project) {
            Ok(path) => located.push((*t, path)),
            Err(e) => {
                eprintln!("{:<14} {e:#}", t.label);
                failed += 1;
            }
        }
    }
    let planned = install::plan_keys(&located, identity);

    let mut keys: Vec<String> = Vec::new();
    for ((t, _), (key, shared_with)) in located.into_iter().zip(planned) {
        if let Some(first) = shared_with {
            println!(
                "{:<14} shares its file with {first}, so it acts as the same key",
                t.label
            );
        }
        match install::install(t, project, &key, explicit_home.as_deref(), dry_run) {
            Ok(o) => {
                let what = if dry_run {
                    if o.changed {
                        "would write"
                    } else {
                        "already set"
                    }
                } else if o.changed {
                    "wrote"
                } else {
                    "already set"
                };
                println!("{:<14} {what} {}", o.tool, o.path.display());
                println!(
                    "{:<14} acts as the agent key {:?}, not as you",
                    "", o.identity
                );
                if !keys.contains(&o.identity) {
                    keys.push(o.identity.clone());
                }
                if let Some(b) = &o.backup {
                    println!("{:<14} kept a copy of the old file at {}", "", b.display());
                }
                if o.changed && !dry_run {
                    if let Some(n) = t.note {
                        println!("{:<14} {n}", "");
                    }
                }
            }
            Err(e) => {
                eprintln!("{:<14} {e:#}", t.label);
                failed += 1;
            }
        }
    }
    if failed > 0 {
        return Ok(1);
    }
    if !dry_run {
        let first = keys.first().map(String::as_str).unwrap_or("<agent>");
        println!(
            "\nAn agent key starts in no rooms. Let it into each room it should see, as the owner:\n\n  \
             diavlos invite <room> {first}\n  \
             diavlos --as {first} join <invite>"
        );
        if keys.len() > 1 {
            println!("\nThe same for {}.", keys[1..].join(", "));
        }
        println!(
            "\nThen restart the tool: diavlos_send, diavlos_next, diavlos_ask and eight more."
        );
    }
    Ok(0)
}

fn short(node: &str) -> String {
    node.chars().take(10).collect()
}

fn print_message(m: &Message, json: bool) -> Result<(), Error> {
    if json {
        println!("{}", serde_json::to_string(m)?);
        return Ok(());
    }
    let mut head = format!("[{}] {} ({})", m.seq, m.from, m.kind);
    if let Some(to) = &m.to {
        head.push_str(&format!(" to {to}"));
    }
    if let Some(r) = &m.reply_to {
        head.push_str(&format!(" re {r}"));
    }
    if m.tombstone {
        println!("{head}: (content removed)");
    } else {
        println!("{head}: {}", m.text);
    }
    Ok(())
}
