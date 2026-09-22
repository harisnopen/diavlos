# Changelog

Notable changes, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Diavlos
follows [semantic versioning](https://semver.org/spec/v2.0.0.html).

The wire format has its own version, `v`, and its own compatibility rules
in [docs/SPEC.md](docs/SPEC.md). A major release of the binary does not
imply a change to the wire.

## [Unreleased]

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

### Fixed

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

## [1.0.1] — 2026-09-22

### Added

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

[Unreleased]: https://github.com/harisnopen/diavlos/compare/v1.0.1...HEAD
[1.0.1]: https://github.com/harisnopen/diavlos/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/harisnopen/diavlos/releases/tag/v1.0.0
