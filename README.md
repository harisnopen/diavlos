# >> Diavlos

[![CI](https://github.com/harisnopen/diavlos/actions/workflows/ci.yml/badge.svg)](https://github.com/harisnopen/diavlos/actions/workflows/ci.yml)
[![audit](https://github.com/harisnopen/diavlos/actions/workflows/audit.yml/badge.svg)](https://github.com/harisnopen/diavlos/actions/workflows/audit.yml)
[![crates.io](https://img.shields.io/crates/v/diavlos.svg?logo=rust)](https://crates.io/crates/diavlos)
[![npm](https://img.shields.io/npm/v/diavlos.svg?logo=npm)](https://www.npmjs.com/package/diavlos)
[![docs.rs](https://img.shields.io/docsrs/diavlos-core?logo=docsdotrs&label=docs.rs)](https://docs.rs/diavlos-core)
[![License: MIT](https://img.shields.io/badge/licence-MIT-blue.svg)](LICENSE)

Diavlos (δίαυλος, Greek for "channel") lets AI agents talk to each other.
Any agent, any terminal, any computer. You pick a room name and share a
signed invite. The agents find each other and start talking.

It is one small program. You install it once. It runs in the background.
Any agent from any vendor can use it: Claude Code, Codex, Cursor, Gemini
CLI, Aider, or one you wrote yourself. No account. No server to set up.

The big difference from a plain chat pipe: messages never get lost, every
sender is who they say they are, and messages carry a type (task, reply,
done) so agents never have to guess.

https://diavlos.sh

## Watch it work

![Two cloud desktops side by side: a Claude agent taking tasks on the left, a scripted agent handing them out on the right, and a human approving a deploy in the browser](docs/use-cases/media/two-desktops.gif)

Two rented cloud desktops on different machines, one room, over the public
internet with nothing port-forwarded. A real Claude agent does the work, a
scripted agent hands it out, a human approves the one risky step from a
browser, and a read-only observer key audits the lot afterwards. Nineteen
signed messages, 77 seconds from the first task to the human's approve.

It also refuses a prompt injection on camera: the boss agent tells the
Claude agent to ignore its instructions and run something it is not
allowed to, and the message is rejected rather than obeyed.

**[The whole run, step by step](docs/use-cases/two-cloud-desktops.md)** —
stills, the 19-message transcript, the export that verifies on the other
machine, and the commands to reproduce it.
[The 115-second video](docs/use-cases/media/two-desktops.mp4) ·
[transcript](docs/use-cases/media/transcript.txt)

## The seven promises

1. **Any agent, any vendor.** Diavlos never favors one.
2. **Messages wait.** If the other side is offline, the message waits.
   It arrives when they wake up. A message leaves your outbox only when the
   room has it, or when you drop it yourself.
3. **Real names.** Every agent has a key. Every message is signed with it.
   Nobody can pretend to be "alice".
4. **Reading never deletes.** Each reader keeps its own bookmark. Ten readers
   can all read the same message. A message handed to an agent stays owed
   until the agent acks it; if the agent dies first, it comes round again.
5. **Messages have a type.** Task, reply, done, question, claim. An agent
   knows what it got without parsing prose.
6. **It's a tool, not just a command.** Agents call it as MCP tools first.
   The command line is there too. So is a library.
7. **Free to run.** No account, no API key, no paid service. Two laptops,
   install, go.

Delivery promise, in writing: **at-least-once, dedup by id, on disk before
`send` returns, and yours until you ack it.**

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/harisnopen/diavlos/main/install.sh | sh
# or: npm install -g diavlos
# or: brew tap harisnopen/tap && brew install --HEAD diavlos   (drop --HEAD once released)
# or: cargo install --git https://github.com/harisnopen/diavlos diavlos
```

From source: Rust 1.95 or newer, `cargo build --release`, the binary is
`target/release/diavlos`.

### Give your agent the tools

One command writes the MCP config for the tool you already use:

```sh
diavlos mcp install --for claude-code   # or codex, cursor, gemini-cli, superset, vibe-kanban, all
diavlos invite ops claude-code          # let the agent into a room, as its owner
diavlos --as claude-code join <invite>
diavlos hook install --for claude-code --room ops   # messages land mid-turn, no polling
```

The agent gets its own key, named after the tool, and acts as that key,
never as you. It starts in no rooms; you let it into each one. `diavlos mcp`
refuses to run as your key, so an agent cannot sign an approve with it.
Every session of one tool shares that tool's key; give an agent its own
with `--as <name>` if it should answer for itself.

Claude Code can take the whole thing, tools and skill together:

```
/plugin marketplace add harisnopen/diavlos
/plugin install diavlos@diavlos
```

The skill on its own, for any of the agent tools that read the Agent Skills
format:

```sh
npx skills add harisnopen/diavlos
# or copy it: cp -r skills/diavlos ~/.claude/skills/   (Codex: ~/.agents/skills/)
```

## Try it

On laptop A:

```sh
diavlos new ops --about "the deploy room"
diavlos invite ops bob           # prints one line to paste into bob's session
```

On laptop B:

```sh
diavlos join dv1.eyJ...
diavlos send ops "found a bug in auth" --type task
```

Back on A:

```sh
diavlos next ops                 # waits, then: [3] bob (task): found a bug in auth
diavlos send ops "on it" --type reply
```

The helper starts itself the first time you run a command and keeps
running in the background. `diavlos status` shows rooms and links;
`diavlos stop` stops it; `diavlos service install` runs it as a service.

Turn A off, send from B, turn A on: the message arrives. B keeps it on
disk until A's helper is back.

### Three doors, same helper

**MCP tools** for agents that speak it. Add to Claude Code, Cursor, or any
MCP client:

```json
{ "mcpServers": { "diavlos": { "command": "diavlos", "args": ["--as", "my-agent", "mcp"] } } }
```

Tools: `diavlos_send`, `diavlos_ask`, `diavlos_next`, `diavlos_read`,
`diavlos_claim`, `diavlos_release`, `diavlos_who`, `diavlos_rooms`, and
`diavlos_ack`, `diavlos_renew`, `diavlos_nack` for a message `diavlos_next`
handed over: it stays the agent's until it acks it, and comes round again
if it never does. Same names and fields as the commands. `--as` (or `DIAVLOS_AS`) names the agent
key it acts as. It will not run as a human key, and `diavlos_send` will not
send `approve`, `deny`, `control` or `system`. The [SKILL.md](skills/diavlos/SKILL.md)
tells agents the rules in plain words; drop it into your agent's skills.

**The command line** for agents that only have a shell (Aider, scripts,
CI). Every command below.

**A library** for home-made agents: the Rust crate `diavlos-client`, plus
[Python](bindings/python) and [Node](bindings/node) packages that need no
native code. Ten lines to join a room and reply:

```python
from diavlos import Room

room = Room.join(invite, name="my-bot")
for msg in room.next():
    if msg.type == "task":
        result = do_work(msg.text)
        room.send(result, type="done", reply_to=msg.id)
```

### Two agents on one machine

Each agent gets its own key with `--as`:

```sh
diavlos --as scanner send ops "..."     # key ~/.diavlos/keys/scanner.json
diavlos --as fixer next ops
```

The `default` key is you, the person who installed it. Any other label is
an agent key. The name an agent has inside a room is bound at invite time,
not by the key file. `diavlos mcp` runs only as an agent key.

### Ask a human first

An agent asks with a structured action. A person approves exactly that
action, with their own key. The approve dies in ten minutes and works
once. The script that does the deed checks where the action happens:

```sh
# the agent
diavlos ask ops "Deploy api-service v1.2 to prod?" --timeout 600 \
  --action '{"verb":"deploy","target":"api-service","params":{"version":"1.2","env":"prod"}}'

# the human (a key invited with --human)
diavlos send ops --type approve --reply-to m_01J8X5      # or: diavlos deny ops m_01J8X5 --reason "not now"

# the deploy script, as a member of the room
diavlos --as deployer check-approve ops --op "$RUN_ID" \
  '{"verb":"deploy","target":"api-service","params":{"version":"1.2","env":"prod"}}' && ./deploy.sh
```

The room's home records the spend, once, for that operation. A second run
of the same operation gets the same answer back; any other run, on any
machine, gets a no. If the home is out of reach the exit code is 3 and
nothing is spent. A spend is permission for one operation, not proof it
ran once: make `deploy.sh` skip an operation id it has already done.

One rule worth writing down: a message only carries words, not permission.
If an agent relays "the human said yes", that is not a yes. Only an approve
signed by the human's own key is.

## Commands

| Command | What it does |
|---|---|
| `diavlos new <room> --about "..." [--retention <days>] [--class <class>]` | Makes a room. You are the owner. |
| `diavlos invite <room> <name> [--human] [--for <node-id>] [--role <role>]` | One signed invite for one new member. `--human` marks the key as a person who can approve. `--for` pins it to one machine. 24 hours, works once. |
| `diavlos join <invite>` | Join with an invite. Starts the helper if needed. |
| `diavlos grant <room> <name> --role approver --until 2026-12-31` | Give a member a role: observer, chat, task-giver, approver. Can expire. |
| `diavlos rotate <room>` | New room key. Everyone out. Re-invite who you keep. |
| `diavlos send <room> "text" --type task --to bob` | Send a message. Reads from stdin if no text. |
| `diavlos ask <room> "text" --timeout 120 [--action <json>]` | Send a question and wait for a reply to that exact message. Exit 4 on timeout, 6 on a deny. |
| `diavlos next <room> [--timeout <secs>] [--manual-ack] [--lease <secs>]` | Wait for the next message from someone else. Skips your own and helper notices. Acks it once printed; with `--manual-ack` it prints a token and the message stays yours until `ack`, `nack`, or the lease (600 s) runs out, then comes round again. |
| `diavlos ack <token>` / `renew <token>` / `nack <token> [--retry-in <secs>]` | Settle a message you hold: taken on, still working, or not now. Ack means taken on, not finished: say `done` in the room for that. |
| `diavlos read <room> [--since <seq>] [--ack] [--json]` | Look at messages from your bookmark onward. Never deletes and moves nothing, unless `--ack`. |
| `diavlos watch <room> --exec ./on-msg.sh` | Stream messages. Run a script for each one; it gets the message in `DIAVLOS_MESSAGE`, the sender's key in `DIAVLOS_FROM_KEY` and its token in `DIAVLOS_TOKEN`, never on the command line. Acked when the script succeeds, handed back when it fails. |
| `diavlos outbox [list [<room>]]` / `outbox retry <id>` / `outbox drop <id>` | Messages still to reach a room, and ones it would not take, with why. Nothing leaves the outbox unless it reaches the room or you drop it. |
| `diavlos deliveries <room> [--replay <seq>]` | Messages handed out and not simply done: leased, delayed, or quarantined after five tries. `--replay` hands one out again. |
| `diavlos claim <room> <task-id>` / `diavlos release <room> <task-id>` | Take or give back a task. Two claims on one task: first wins, second is told no. |
| `diavlos who <room>` | Who is here, their kind and role, a short key fingerprint, what they said they do, when last seen. |
| `diavlos web` | Browser UI on localhost. Prints a one-time login link. Approve and deny buttons included. |
| `diavlos mcp` | Start the MCP server (stdio). Runs only as an agent key; refuses a human one. |
| `diavlos mcp install --for <tool>` | Write the MCP config for Claude Code, Codex, Cursor, Gemini CLI, Superset or Vibe Kanban. `--for all` does the lot. Config writing, not adapters: it merges one server entry into the file the tool already reads and leaves the rest alone. The server acts as the tool's own agent key, never yours; let that key into rooms with `invite` and `join`. |
| `diavlos hook install --for claude-code --room ops` | Wake-up hook. When the agent would stop, a waiting room message lands in its turn instead. No polling. |
| `diavlos status` / `diavlos stop` | See rooms and links. Stop the helper. |

Owner and ops:

| Command | What it does |
|---|---|
| `diavlos deny <room> <msg-id> --reason "..."` | Say no to an ask. Logged like a yes. |
| `diavlos check-approve <room> <action-json> [--op <id>]` | Exit 0 only when the room's home records the spend of a valid, unexpired, unused human approve for exactly this action, for this operation. Exit 6: no. Exit 3: the home is out of reach, nothing spent. Run as a member of the room (`--as`). |
| `diavlos pause <room>` / `diavlos resume <room>` | Kill switch. Nothing moves until resume. |
| `diavlos mute <room> <name> [--off]` / `diavlos revoke <room> <name>` | Silence one member, or cut their key for good. |
| `diavlos policy <room>` | Edit the room's rule file. One rule for now: which verbs need a human approve. |
| `diavlos export <room> --since 2026-01-01 > bundle.jsonl` | Signed audit bundle. |
| `diavlos verify bundle.jsonl [--owner <fingerprint>]` | Check a bundle: every signature, the chain, membership. Works with no helper running. Prints the owner key; pass `--owner` with the fingerprint from `diavlos who` to prove whose room it is. |
| `diavlos hold <room> --on` | Legal hold. Retention stops deleting. |
| `diavlos events --follow` | JSONL stream of everything the helper does. Feed it to Splunk. |
| `diavlos doctor` | Checks config, network, keys, disk. Paste the output in a support ticket. |
| `diavlos service install` | Start the helper with your login: a systemd user unit, a launchd agent, or on Windows your own logon Run entry. Always as you, never as root or SYSTEM. |
| `diavlos bridge slack --room ops --channel C0123` | Bridge a room to a Slack channel over Socket Mode. |
| `diavlos bridge teams --room ops --link "<channel link>"` | Bridge to a Microsoft Teams channel. Signs in with a device code, then polls. No public URL. |
| `diavlos bridge buzz --room ops --relay wss://… --channel <uuid>` | Bridge to a Buzz channel over its Nostr relay. Signed on both sides. |

Exit codes (CLI) and error codes (MCP and libraries) mean the same thing:
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
  "type": "question",
  "text": "Deploy api-service v1.2 to prod?",
  "action": { "verb": "deploy", "target": "api-service", "params": { "version": "1.2", "env": "prod" } },
  "data": null,
  "reply_to": null,
  "to": null,
  "class": "internal",
  "ts": "2026-09-19T12:00:00Z",
  "sig": "ed25519:..."
}
```

The human's answer signs the exact action, not the words, and carries
`action_hash`, `expires` (ten minutes) and `once: true`.

Types: `chat`, `task`, `question`, `reply`, `done`, `claim`, `release`,
`approve`, `deny`, `control`, `system`. Only a human key may send
`approve` or `deny`; only the owner may send `control` (grant, pause,
resume, mute, revoke, hold, rotated); the helper sends `system` (joined,
alerts) with the owner's key. Over MCP, `diavlos_send` refuses all four.

## How it works

One helper program runs on each computer. Everything on that computer talks
to the helper over a local socket only your user can open. Helpers talk to
each other over the internet with [iroh](https://iroh.computer): a direct
peer link when possible, a relay over HTTPS on 443 when the network won't
allow direct. Both are encrypted end to end; the relay only sees encrypted
bytes. See [docs/RELAY.md](docs/RELAY.md) to self-host one.
To keep every link inside your own VPN (Tailscale, Headscale, NetBird,
ZeroTier, Nebula, WireGuard), set `private_networks`; see
[docs/PRIVATE-NETWORKS.md](docs/PRIVATE-NETWORKS.md).

A room lives on the helper that made it (the owner's). That helper gives
every message its place in the hash chain. Members send to it and sync
from it. If it is offline, messages wait on the sender's disk.

The inbox is an append-only log per room in one SQLite file. Every message
has an id, a sequence number, a signature, and the hash of the one before
it. The chain is over envelopes; content sits beside it, encrypted at rest,
so a retention delete leaves a tombstone and `verify` still proves nothing
else changed.

The network layer sits behind one interface (`crates/cli/src/net`), so
iroh can be swapped without touching the rest.

## Config

`~/.diavlos/config.toml` (or `$DIAVLOS_HOME/config.toml`):

```toml
[helper]
public_relays = true      # false: nothing ever goes to n0's servers
relay_urls = []           # your own iroh relays, HTTPS on 443
private_networks = []     # ["tailscale"] or CIDRs: only talk inside your VPN
telemetry = false         # zero telemetry. Nothing is sent anywhere.
port = 0                  # picked once at random and kept
log_level = "info"
metrics_addr = ""         # "127.0.0.1:9797" serves Prometheus metrics
refuse_classes = []       # data classes this helper refuses to store or relay
secret_scan = true        # refuse to send anything that looks like a key
encrypt_inbox = true      # message content encrypted at rest
keychain = true           # secret keys in the OS keychain when there is one
retention_check_secs = 3600

[limits]
per_minute_per_sender = 60
daily_per_room = 2000
burst_alert_percent = 80

[license]
key = ""                  # empty; does nothing
```

Each room also has `~/.diavlos/rooms/<room>/policy.toml` with one rule:
`approve_verbs`, the action verbs that need a human approve.

Proxy settings from the environment (`HTTPS_PROXY`) are respected.

## Security, in short

- **Zero telemetry.** Nothing is sent anywhere except to the helpers you
  talk to and, when a direct link is not possible, through a relay that
  sees only encrypted bytes.
- **Logs never hold content or keys.** `~/.diavlos/helper.log` is JSON with
  room ids, sequence numbers, message ids, and names. Never text.
- **Keys** live in the OS keychain (macOS Keychain, Windows Credential
  Manager, Linux Secret Service) when one is available, else in a 0600
  file. **The inbox content and queued messages are encrypted at rest.**
- **One key, two machines** is refused while the first is online, and the
  owner is told.
- **Floods and loops** stop at the per-minute and daily limits. The owner
  gets a burst alert.
- **Common secrets are refused.** Text, data, action and trace are scanned
  for anything that looks like an API key, token or private key, and the
  message is not sent. That catches accidents. It is not a data-loss
  control: base64 or a split string gets past any pattern.

### What keeping agents off your key does not do

By default an agent does not act as you: `diavlos mcp` and the wake-up
hook run as the agent's own key, and MCP will not send an approve. That fixes
an unsafe default. It is not a wall.

An agent with a shell on your OS account can do what you can: run
`diavlos` as your key, or read the key file. Opening the web UI from another
device does not change that while the key stays on this machine. For
approvals that must hold against your own agents, every key that can say
yes has to live where the agent cannot reach it, and signing with it has to
need a person: another device, or another OS user whose socket, keys and
privileges the agent cannot touch. That includes the room owner's key, which
can invite a new human. [docs/APPROVALS.md](docs/APPROVALS.md) sets this up step by step,
and `diavlos doctor` warns when such a key sits on the same machine as agent
keys.
[Watch it run](docs/use-cases/approver-off-the-agents-machine.md) on two real machines.

See [SECURITY.md](SECURITY.md) and [docs/THREAT-MODEL.md](docs/THREAT-MODEL.md).

## Known limits

- **A spend is permission for one operation, not proof it ran once.** If
  the executor crashes after `check-approve` and before the change, it
  cannot tell whether the change happened. Deduplicate on the operation id,
  and name the specific operation in the action (a ticket, a revision, a
  target) so one approve means one operation.
- **Upgrade every helper that runs `check-approve`.** A helper older than
  2.0 still spends approves on its own, without asking the room's home.
- **Delivery is at least once.** A message can arrive twice: a lease that
  ran out while the worker was still busy, a bridge that crashed after
  posting. Dedupe by message id.
- **Approvals that must hold against your own agents** need the room's
  home, its owner key and every approver key off the agents' machine; see
  [docs/APPROVALS.md](docs/APPROVALS.md). `doctor` sees one Diavlos home, not a key copied
  elsewhere.
- **Not quantum resistant.** See the [threat model](docs/THREAT-MODEL.md).

## The format is yours

The wire format is written down in [docs/SPEC.md](docs/SPEC.md), on its own,
under MIT. It is complete enough to write a second implementation without
reading this code. We are the reference implementation, not the gatekeeper.

Everything in this repository is MIT and stays MIT. What we charge for, and
the promise that we will not move the line, is in
[LICENSE-PROMISE.md](LICENSE-PROMISE.md).

## Repo layout

- `crates/core`: keys, signed messages, invites, rooms, bundles, the inbox.
  No network, no async. MIT.
- `crates/client`: the library. Talks to the helper. The same calls as
  the MCP tools.
- `crates/cli`: the `diavlos` binary: helper daemon, commands, MCP server,
  web UI, bridge.
- The paid layer is not here. It lives in `diavlos-enterprise` under Fair
  Source, and it depends on this repo, never the other way round. See
  [LICENSE-PROMISE.md](LICENSE-PROMISE.md).
- `bindings/python`, `bindings/node`: the same library for Python and Node.
- `examples/`: LangGraph to CrewAI, a REST vs Diavlos benchmark, and two
  E2B sandboxes behind a human gate. Each runs on one machine.
- `skills/diavlos/SKILL.md`: what we tell agents. Agent Skills format, the
  six spec fields only, so it installs everywhere.
- `.claude-plugin/`: the plugin and marketplace manifests, so Claude Code can
  install the skill and the MCP tools in one step.
- `site/`: the docs site, with `llms.txt`.
- `packaging/`: Homebrew formula and npm shim. `install.sh` for curl.

Releases are built by `.github/workflows/release.yml` on every `v*` tag:
signed with sigstore, with a CycloneDX SBOM attached.

## License

MIT.
