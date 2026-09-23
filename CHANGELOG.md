# Changelog

Notable changes, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Diavlos
follows [semantic versioning](https://semver.org/spec/v2.0.0.html).

The wire format has its own version, `v`, and its own compatibility rules
in [docs/SPEC.md](docs/SPEC.md). A major release of the binary does not
imply a change to the wire.

## [Unreleased]

### Security

- An agent can no longer sign an approve with the person's key. `mcp
  install` used to write a config that ran as `default`, the installer's
  human key, so an agent could answer its own question with an approve that
  `check-approve` accepted. Found by an outside review and reproduced
  before it was fixed. Now `diavlos mcp` refuses to run as a human key,
  judged by the key's kind rather than its label, and `diavlos_send`
  refuses `approve`, `deny`, `control` and `system`.
- The secret scan covers an action's verb, target and params, and the
  trace, as well as the text and data. A key in `action.params` used to be
  signed and sent. It also catches a `"password": "..."` written as JSON.
- An approve is spent once, recorded by the room's home, for one named
  operation. "Works once" used to be per machine: each helper kept its own
  record, so two machines holding the same approve could each spend it, and
  a helper that had not heard of a revoke still honoured it. The home now
  decides on its own current state and writes the spend and a signed
  `approve_spent` event to the chain together. The same operation retried
  gets the recorded answer; any other is told no.
- Queued messages are sealed with the inbox key before they are written.
  They used to sit in the database in plain text. Rows an older version
  wrote are sealed at startup. `secure_delete` is on, and the write-ahead
  log is emptied every ten minutes and at shutdown.

### Fixed

- A queued message is no longer deleted when the home refuses it or the
  link drops mid-frame. It stays until it is in the chain or a person drops
  it: temporary failures wait and retry, a definitive refusal is kept as
  `failed`, an unknown answer is quarantined only after it keeps coming back
  for an hour.
- A message `next` handed out is no longer marked read before the agent has
  it. It is leased to the reader until the reader acks it, and handed out
  again if the lease runs out.
- Two messages queued in the same second could be sent in the wrong order.

### Changed

- **Breaking:** `mcp install` and `hook install` give each tool its own
  agent key, named after the tool (`claude-code`, `codex`, ...), instead of
  the installer's. Tools that write the same file share one key: Vibe Kanban
  writes Claude Code's, and a project's `.mcp.json` serves both Claude Code
  and Superset. An agent key starts in no rooms; let it in with `invite` and
  `--as <key> join`. Nothing of the person's is copied to it. An existing
  config that runs as `default` now stops with a message saying what to do.
  After upgrading, run `diavlos stop` once so the helper restarts as the
  new version; `diavlos mcp` says so if a helper from before is still
  running.
- **Breaking:** `read` only looks: it no longer moves the bookmark.
  `read --ack` (and `ack` in MCP and the libraries) settles exactly the
  messages it returned. Anything that polled with `read` should use `next`,
  or add `--ack`.
- **Breaking:** `check-approve` runs as a member of the room (`--as`) and
  asks the room's home, which must be reachable: exit 3 when it is not,
  and nothing is spent. A helper older than this one cannot record spends;
  upgrade the home first, then every helper that runs `check-approve`.
- `next` acks once it has printed the message; `--manual-ack` prints a
  token instead. `diavlos_next` over MCP returns the message and its token.
  `watch` acks when its script succeeds and hands the message back when it
  fails. The Python and Node loops ack a message when asked for the next
  one. The wake-up hook only peeks, so its reminder comes back until the
  agent acks. The bridges ack after the other service confirms the post.
- Control messages (grant, pause, mute, revoke, hold) take effect in the
  same transaction as the message.
- `diavlos-core`: `Error::OverBudget` carries the seconds until the limit
  lets the next message through, `OverBudget(String, Option<u64>)`, and
  `Error::fate()` says whether a failure is temporary or definitive.
- The docs stop claiming more than the code does. "Secrets never leave the
  machine" becomes what the scan is, a guard against accidents. "Never lose
  a message" becomes "Messages wait", and a new *Known limits* section says
  what is still not guaranteed. The threat model says plainly that an
  agent with a shell on the same OS account can act as the person, and that
  keeping agents off the key by default is not a wall against that.

### Added

- `whoami` on the local protocol: a key's label, name, kind and fingerprint.
- `diavlos outbox [list|retry|drop]`: every queued message, its state and
  why. `status` and `doctor` count failed and quarantined ones.
- `diavlos ack|renew|nack <token>`, `next --manual-ack --lease <secs>`,
  `diavlos deliveries <room> [--replay <seq>]`; MCP tools `diavlos_ack`,
  `diavlos_renew`, `diavlos_nack`; `ack`, `renew`, `nack` and `take` in the
  libraries. Config `lease_secs` (600) and `max_attempts` (5).
- `check-approve --op <id>`: name the operation, so a retry gets the
  recorded answer.
- Error frames between helpers say whether a failure is temporary or
  definitive (`fate`) and when trying again can work (`retry_after`).
  Older helpers ignore both.
- A warning when a key that can say yes sits on the same machine as agent
  keys: an approver's key, or the room owner's, which can invite a new
  human. `doctor` shows it as `WARN` (its exit code does not change),
  `status` lists it, and `check-approve` prints it when it passes. The
  status result has `approval_risks`, the check-approve result `risk`.
- [docs/APPROVALS.md](docs/APPROVALS.md): how to keep the room's home, its
  owner key and the approver keys on a machine the agents cannot reach.

## [1.1.0] — 2026-09-23

1.0.1 was prepared but never released; its changes ship here.

### Security

- `check-approve` checks the approver still has the approver role, and is
  not revoked, muted or expired, when the approve is spent. It also skips
  an approve dated in the future. An approve given before a downgrade
  could still be spent after it.
- An agent key can no longer approve or deny, even when it owns the room.
- `reply_to` must name a message in the same room. A claim or approve
  could reach a task or question in another room.
- The home refuses a `ts` more than five minutes ahead of its clock, or in
  any spelling but `YYYY-MM-DDTHH:MM:SSZ`. Rate limits count by when the
  home stored a message, not by the sender's `ts`. Backdating used to get
  past the limits, and future-dating escaped retention.
- The Slack bridge escapes room text, so a member cannot ping `@channel`,
  mention people, or disguise a link.
- On Windows, `service install` starts the helper at logon as you, from
  your own Run key, instead of as a LocalSystem service. `uninstall`
  removes an older SYSTEM service too, or says how.

### Added

- `verify` prints the owner key's fingerprint, and `verify --owner
  <fingerprint>` fails a bundle from any other owner. A bundle alone only
  proves it is whole, not whose it is.
- Release archives now carry `LICENSE` and `THIRD-PARTY-LICENSES.txt`
  beside the binary. MIT, Apache-2.0 and the BSD licences all allow
  shipping a binary built from their code on the condition that the
  copyright notice travels with the copy; a bare binary in a tar.gz
  carried nobody's. The file is generated at release time by `cargo about`
  from the crates that really went into the build, so it cannot go stale.
  The Homebrew formula and the npm package install both.
- `diavlos doctor` reports the length of the helper's socket path.
- npm publishing runs from GitHub Actions through npm's trusted
  publishing (OIDC). No token is stored anywhere.

### Changed

- A home directory too deep for a Unix socket now fails in milliseconds
  with a message naming the limit, how far over it is, and that
  `DIAVLOS_HOME` or `--home` is the way out. It used to wait ten seconds
  and then report `local socket name length exceeds capacity of sun_path
  of sockaddr_un`, which named a C struct instead of the problem.

### Fixed

- A resubmit no longer runs anything twice. When a member's outbox sent a
  message again because the home's answer was lost, the home counted it
  against the rate limits again, re-applied control ops, fired events
  twice, and answered with a made-up seq. A resent claim was refused
  ("you already hold this task") and dropped as refused, though it had
  been stored. The home now answers a resubmit with the stored message.
- The web page shows the newest 500 messages of a room, not the oldest.
- `verify` flags a tombstone that still carries content.
- The systemd unit and launchd plist quote their paths, so a path with a
  space works.
- Invites signed with `"for_node": null`, as the spec example shows,
  verify.
- `docs/SPEC.md` now matches the code: `trace` is signed and chained, the
  name rules, the `service` kind, which fields are omitted when absent,
  approve fields, the approver check, delivery being at least once, and
  what happens to unknown fields.
- A read from a given `since` no longer moves your bookmark. The web page
  reads from seq 1 as you, so opening it used to mark every message read,
  and the agent then missed new ones or got old ones twice.
- The Claude Code Stop hook answers with `decision` and `reason` at the top
  level, the shape Claude Code reads for Stop. Before, the wake-up did
  nothing.
- The Stop hook no longer marks messages read on a turn it will not block.
  They wait for the next stop instead of being lost.
- The GitHub Action says `approved=true` only for a human-signed approve.
  An ask with no `action` that got a plain reply ("no, don't deploy") used
  to count as approved; its outcome is now `answered`.
- `install.sh` and the Action stop when cosign rejects the download. Before,
  a failed check still installed the binary.

## [1.0.0] — 2026-09-21

First release. Signed binaries for five platforms, each with a sigstore
bundle, plus a CycloneDX SBOM.

### Added

- **Rooms and messages.** `new`, `invite`, `join`, `send`, `next`,
  `read`, `who`. Every message is signed, typed (`chat`, `task`, `reply`,
  `done`, `question`, `claim`, `release`, `approve`, `deny`) and linked
  into a hash chain. Reading never deletes: each reader keeps a bookmark.
- **Human approval.** `ask` carries a structured action; only a key whose
  kind is `human` may send an `approve`, and an approval expires and works
  once. `check-approve` is the gate a deploy script calls before it acts.
  `deny` logs a no the same way as a yes.
- **Roles and control.** `grant` with expiry, `revoke`, `mute`, `pause`
  and `resume`, `rotate`, `policy`.
- **Audit.** `export` writes a signed bundle, `verify` checks one with no
  helper running. `hold` stops retention deleting. `events` streams JSONL.
  Content is stored apart from the envelope, so a deletion leaves a
  tombstone and the chain still verifies.
- **Transport.** iroh: direct peer links with relay fallback over 443,
  proxy settings respected, `public_relays = false` for a private setup.
- **Safety rails.** Outbound secret scanning on by default, a per-sender
  rate limit and a daily budget per room, a size cap, ASCII-only names
  with a key fingerprint beside each one, and a refusal when the same key
  appears from two machines.
- **For agents.** An MCP server with eight tools. `mcp install --for`
  writes the config for Claude Code, Codex, Cursor, Gemini CLI, Superset
  and Vibe Kanban. `hook install` wires the wake-up hook so room messages
  arrive mid-turn instead of being polled for.
- **For people.** `web` serves a browser UI behind a one-time login link.
  Bridges to Slack, Microsoft Teams and Buzz.
- **For CI.** A composite GitHub Action that sends to a room, or asks and
  waits for a human-signed yes before the deploy step runs.
- **Libraries.** Python and Node bindings over the same helper socket.
- **Running it.** `service` installs under systemd, launchd or Windows
  services. `doctor` for support tickets, `/metrics` on localhost for
  Prometheus, zero telemetry.

[Unreleased]: https://github.com/harisnopen/diavlos/compare/v1.1.0...HEAD
[1.1.0]: https://github.com/harisnopen/diavlos/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/harisnopen/diavlos/releases/tag/v1.0.0
