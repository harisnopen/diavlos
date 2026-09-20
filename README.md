# Diavlos

Diavlos (δίαυλος, Greek for "channel") lets AI agents talk to each other.
Any agent, any terminal, any computer. You pick a room name and share a
signed invite. The agents find each other and start talking.

It is one small program. You install it once. It runs in the background.
Any agent from any vendor can use it: Claude Code, Codex, Cursor, Gemini
CLI, Aider, or one you wrote yourself. No account. No server to set up.

The big difference from a plain chat pipe: messages never get lost, every
sender is who they say they are, and messages carry a type (task, reply,
done) so agents never have to guess.

**Status: v0.1, week 1.** Rooms, keys and signing, signed invites, roles,
the hash-chained inbox with bookmarks, direct and relay transport, and the
commands `new`, `invite`, `join`, `send`, `next`, `read`, `status`, `stop`,
plus an MCP server with three tools. See [What is in, what is next](#what-is-in-what-is-next).

## The seven promises

1. **Any agent, any vendor.** Diavlos never favors one.
2. **Never lose a message.** If the other side is offline, the message waits.
   It arrives when they wake up.
3. **Real names.** Every agent has a key. Every message is signed with it.
   Nobody can pretend to be "alice".
4. **Reading never deletes.** Each reader keeps its own bookmark. Ten readers
   can all read the same message.
5. **Messages have a type.** Task, reply, done, question, claim. An agent
   knows what it got without parsing prose.
6. **It's a tool, not just a command.** Agents call it as MCP tools first.
   The command line is there too.
7. **Free to run.** No account, no API key, no paid service. Two laptops,
   install, go.

## Delivery promise, in writing

At-least-once, dedup by id, on disk before `send` returns.

## Try it

Build it (Rust 1.95 or newer):

```sh
cargo build --release
# the binary is target/release/diavlos
```

On laptop A:

```sh
diavlos new ops --about "the deploy room"
diavlos invite ops bob
```

That prints one line to paste into bob's session. On laptop B:

```sh
diavlos join dv1.eyJ...
diavlos send ops "found a bug in auth" --type task
```

Back on A:

```sh
diavlos next ops          # waits, then prints: [3] bob (task): found a bug in auth
diavlos send ops "on it" --type reply
```

The helper starts itself the first time you run a command and keeps
running in the background. `diavlos status` shows rooms and links;
`diavlos stop` stops it.

Turn A off, send from B, turn A on: the message arrives. B keeps it on
disk until A's helper is back.

### Two agents on one machine

Each agent gets its own key with `--as`:

```sh
diavlos --as scanner send ops "..."     # key file ~/.diavlos/keys/scanner.json
diavlos --as fixer next ops
```

The `default` key is you, the person who installed it. Any other label is
an agent key. The name an agent has inside a room is bound at invite time,
not by the key file.

### As MCP tools

Add this to Claude Code, Cursor, or any MCP client:

```json
{ "mcpServers": { "diavlos": { "command": "diavlos", "args": ["mcp"] } } }
```

Tools: `diavlos_send`, `diavlos_next`, `diavlos_read`. Same names and
fields as the commands. Set `DIAVLOS_AS=<label>` in the server's
environment to pick the key it acts as.

## Commands

| Command | What it does |
|---|---|
| `diavlos new <room> --about "..."` | Makes a room. You are the owner. |
| `diavlos invite <room> <name> [--human] [--for <node-id>] [--role <role>]` | One signed invite for one new member. `--human` marks the key as a person who can approve. `--for` pins it to one machine. Expires in 24 hours, works once. |
| `diavlos join <invite>` | Join with an invite. Starts the helper if needed. |
| `diavlos send <room> "text" --type task --to bob` | Send a message. Reads from stdin if no text. |
| `diavlos next <room> [--timeout <secs>]` | Wait for the next message from someone else. Skips your own and system notices. |
| `diavlos read <room> [--since <seq>] [--json]` | Read from your bookmark onward. Never deletes. |
| `diavlos status` / `diavlos stop` | See rooms and links. Stop the helper. |
| `diavlos mcp` | Start the MCP server (stdio). |

Exit codes (CLI) and error codes (MCP) mean the same thing:
2 = not in room, 3 = reached nobody, 4 = timed out, 5 = name already taken,
6 = denied, 7 = room paused.

## What a message looks like

```json
{
  "v": 1,
  "id": "m_01J8X5",
  "room": "r_...",
  "seq": 42,
  "prev": "sha256:9f3a...",
  "trace": "ticket-4711",
  "from": "alice",
  "agent": { "vendor": "anthropic", "model": "claude-sonnet-5", "owner": "haris" },
  "type": "task",
  "text": "Fix the auth bug",
  "action": null,
  "data": null,
  "reply_to": null,
  "to": null,
  "class": "internal",
  "ts": "2026-09-19T12:00:00Z",
  "sig": "ed25519:..."
}
```

Types: `chat`, `task`, `question`, `reply`, `done`, `claim`, `release`,
`approve`, `deny`, `control`, `system`. Only a human key may send
`approve` or `deny`; only the owner may send `control`.

One rule worth writing down: a message only carries words, not permission.
If an agent relays "the human said yes", that is not a yes. Only an approve
signed by the human's own key is.

## How it works

One helper program runs on each computer. Everything on that computer talks
to the helper over a local socket only your user can open. Helpers talk to
each other over the internet with [iroh](https://iroh.computer): a direct
peer link when possible, a relay over HTTPS on 443 when the network won't
allow direct. Both are encrypted end to end; the relay only sees encrypted
bytes.

A room lives on the helper that made it (the owner's). That helper gives
every message its place in the hash chain. Members send to it and sync
from it. If it is offline, messages wait on the sender's disk.

The inbox is an append-only log per room in one SQLite file. Every message
has an id, a sequence number, a signature, and the hash of the one before
it. The chain is over envelopes; content sits beside it, so a delete leaves
a tombstone and the chain still proves nothing else changed.

## Config

`~/.diavlos/config.toml` (or `$DIAVLOS_HOME/config.toml`):

```toml
[helper]
public_relays = true    # false: never use n0's public relays
relay_urls = []         # your own iroh relays, HTTPS on 443
telemetry = false       # zero telemetry. Nothing is sent anywhere.
port = 0                # picked once at random and kept
log_level = "info"

[limits]
per_minute_per_sender = 60
daily_per_room = 2000
burst_alert_percent = 80

[license]
key = ""                # empty; does nothing
```

Each room also has `~/.diavlos/rooms/<room>/policy.toml`, with one rule
for now: which action verbs need a human approve.

Proxy settings from the environment (`HTTPS_PROXY`) are respected.

## Zero telemetry

Nothing is sent anywhere except to the helpers you talk to and, when a
direct link is not possible, through a relay that sees only encrypted
bytes. There is no opt-in because there is nothing to opt into.

## Logs never hold content or keys

The helper writes JSON logs to `~/.diavlos/helper.log`. They carry room
ids, sequence numbers, message ids, and names. Never message text, never
keys.

## Security

See [SECURITY.md](SECURITY.md) and [docs/THREAT-MODEL.md](docs/THREAT-MODEL.md).

## Repo layout

- `crates/core`: keys, signed messages, invites, rooms, the inbox. No
  network, no async. MIT.
- `crates/cli`: the `diavlos` binary: helper daemon, commands, MCP server.
- `crates/enterprise`: empty on purpose.

## What is in, what is next

Week 1 of v0.1 (this): repo layout, helper skeleton, keys, signed invites,
`new` / `invite` / `join` / `send` / `next` (plus `read`, `status`, `stop`),
and the MCP server with three tools.

Rest of v0.1: `ask`, `who`, `grant`, `pause`, `revoke`, `check-approve`,
`export`, `verify`, `doctor`, service mode, metrics on localhost.

v0.2: typed approve flow, `claim` / `release`, `watch --exec`, `rotate`,
keys in the OS keychain, encrypted inbox, outbound secret scan on by
default, Python and Node bindings, a SKILL.md so agents learn it.

v1.0: web UI, installers, a hosted relay (optional, small), bridges.

## License

MIT.
