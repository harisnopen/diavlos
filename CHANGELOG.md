# Changelog

Notable changes, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Diavlos
follows [semantic versioning](https://semver.org/spec/v2.0.0.html).

The wire format has its own version, `v`, and its own compatibility rules
in [docs/SPEC.md](docs/SPEC.md). A major release of the binary does not
imply a change to the wire.

## [Unreleased]

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

[Unreleased]: https://github.com/harisnopen/diavlos/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/harisnopen/diavlos/releases/tag/v1.0.0
