//! `diavlos`: one binary. Helper, CLI, and MCP server.

mod client;
mod config;
mod helper;
mod mcp;
mod net;
mod paths;
mod proto;

use std::io::Read;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use diavlos_core::{DataClass, Error, Message, MessageType, Role};

use client::Client;
use paths::Paths;
use proto::{DraftWire, InviteResult, JoinResult, ReadResult, Request, SendResult, StatusResult};

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
    Join { invite: String },
    /// Send a message. Reads from stdin if no text.
    Send {
        room: String,
        text: Option<String>,
        /// chat, task, question, reply, done, claim, release
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
    /// Wait for the next message from someone else. Skips your own and
    /// system notices.
    Next {
        room: String,
        /// Give up after this many seconds (exit code 4). Default: wait.
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    /// Read from your bookmark onward. Never deletes.
    Read {
        room: String,
        #[arg(long)]
        since: Option<u64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// See rooms and peers.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Stop the helper.
    Stop,
    /// Start the MCP server (stdio). Add this one line to Claude Code or
    /// Cursor config.
    Mcp,
    /// Run the helper in the foreground. Commands start it for you;
    /// this is for services and debugging.
    Helper,
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
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            e.code()
        }
    };
    std::process::exit(code);
}

async fn run(cli: Cli, paths: Paths) -> Result<(), Error> {
    let identity = cli.identity.clone();
    let client = Client::new(paths.clone());
    match cli.cmd {
        Cmd::Helper => helper::run(paths)
            .await
            .map_err(|e| Error::Other(format!("{e:#}"))),
        Cmd::Mcp => mcp::serve(paths, identity)
            .await
            .map_err(|e| Error::Other(format!("{e:#}"))),
        Cmd::New { room, about } => {
            let v = client
                .call(&Request::NewRoom {
                    name: room.clone(),
                    about,
                    identity,
                })
                .await?;
            let r: diavlos_core::Room = serde_json::from_value(v)?;
            println!("made room {} ({}). You are the owner.", r.name, r.id);
            println!("Invite someone: diavlos invite {} <name> [--human]", r.name);
            Ok(())
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
            Ok(())
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
            Ok(())
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
            let v = client
                .call(&Request::Send {
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
                })
                .await?;
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
            Ok(())
        }
        Cmd::Next {
            room,
            timeout,
            json,
        } => {
            let v = client
                .call(&Request::Next {
                    room,
                    identity,
                    timeout_secs: timeout.unwrap_or(0),
                })
                .await?;
            let m: Message = serde_json::from_value(v)?;
            print_message(&m, json)?;
            Ok(())
        }
        Cmd::Read {
            room,
            since,
            limit,
            json,
        } => {
            let v = client
                .call(&Request::Read {
                    room,
                    identity,
                    since,
                    limit,
                })
                .await?;
            let r: ReadResult = serde_json::from_value(v)?;
            if json {
                for m in &r.messages {
                    println!("{}", serde_json::to_string(m)?);
                }
            } else {
                for m in &r.messages {
                    print_message(m, false)?;
                }
            }
            Ok(())
        }
        Cmd::Status { json } => {
            let v = client.call(&Request::Status).await?;
            let s: StatusResult = serde_json::from_value(v)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&s)?);
                return Ok(());
            }
            println!(
                "diavlos {}  node {}  home {}",
                s.version, s.node, s.home_dir
            );
            if s.rooms.is_empty() {
                println!("no rooms yet. Try: diavlos new <room>");
            }
            for r in &s.rooms {
                println!(
                    "  {:<20} {:<7} {:<9} {} members, {} messages{}{} (you: {})",
                    r.name,
                    if r.home { "home" } else { "member" },
                    if r.connected { "linked" } else { "offline" },
                    r.members,
                    r.messages,
                    if r.queued > 0 {
                        format!(", {} queued", r.queued)
                    } else {
                        String::new()
                    },
                    if r.paused { ", paused" } else { "" },
                    r.me.join(", "),
                );
            }
            Ok(())
        }
        Cmd::Stop => {
            match client.call_if_running(&Request::Stop).await? {
                Some(_) => println!("helper stopping"),
                None => println!("helper is not running"),
            }
            Ok(())
        }
    }
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
    println!("{head}: {}", m.text);
    Ok(())
}
