# Contributing

Thanks for looking. Diavlos is a small Rust workspace and the setup is
short.

## Getting it running

```bash
git clone https://github.com/harisnopen/diavlos
cd diavlos
cargo build
cargo test --workspace
```

Rust 1.95 or newer. `rust-toolchain.toml` picks the right toolchain and
pulls in `rustfmt` and `clippy`, so `rustup` will sort itself out on the
first build.

To try your build without touching an existing install:

```bash
DIAVLOS_HOME=/tmp/dv ./target/debug/diavlos new test
DIAVLOS_HOME=/tmp/dv ./target/debug/diavlos send test "hello" --type task
DIAVLOS_HOME=/tmp/dv ./target/debug/diavlos read test
```

Keep that path short. A Unix socket path cannot exceed 108 bytes on Linux
or 104 on macOS, and the home directory sits inside it. `diavlos doctor`
checks this, among other things.

## Before you open a pull request

The same three commands CI runs:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All three have to pass. Warnings are errors here; that is deliberate.

CI runs them again on Linux, macOS and Windows. Windows and macOS have
each caught a real portability bug that Linux did not, so do not be
surprised if something passes locally and fails there.

## What the layout means

| Crate | What lives there |
|---|---|
| `crates/core` | Keys, signing, the message envelope, the hash chain, invites, rooms, the SQLite store. No I/O beyond the database. |
| `crates/client` | Talking to the local helper over its socket. The library other programs use. |
| `crates/cli` | The `diavlos` binary: the helper daemon, every command, the MCP server, the web UI, the bridges. |

The dependency arrow points one way: `cli` → `client` → `core`. Nothing
points back.

Also in the tree: `bindings/` (Python and Node), `packaging/` (Homebrew
formula, npm shim), `skills/` (the Agent Skill), `site/` (the docs site),
`docs/` (the spec, the threat model, publishing).

## Changes to the wire format

`docs/SPEC.md` is the published message format, and other people write
against it. If a change alters what goes on the wire, the spec changes in
the same pull request. A new field is fine; changing the meaning of an
existing one is not, unless the version goes with it.

## Tests

Unit tests sit beside the code in `#[cfg(test)] mod tests`. Two
end-to-end suites live in `crates/cli/tests/`: `e2e.rs` runs two real
helpers against each other, and `plan.rs` walks the features the build
plan promised.

A test that would have caught the bug is worth more than a test that
covers the fix. Where a constant mirrors something the operating system
decides, test it against the real thing rather than against itself —
`the_kernel_really_does_draw_the_line_there` in `crates/client/src/paths.rs`
is the pattern.

## Commit messages

A short line saying what changed, then a blank line, then why. Say what
the old behaviour was if you are fixing something; that is the part
nobody can reconstruct later.

Commits made with AI assistance carry a `Co-Authored-By:` trailer. That
is on purpose. A good deal of this repository was written that way, every
line of it reviewed and tested, and pretending otherwise would be the
dishonest option.

## Security

Do not open a public issue for a vulnerability. `SECURITY.md` has the
private route. `docs/THREAT-MODEL.md` explains what Diavlos does and does
not defend against, and is worth reading before reporting.

## What the project will not take

- A dependency on anything in the paid layer. `LICENSE-PROMISE.md`
  explains why the arrow only points one way.
- Telemetry, analytics or a phone-home of any kind, on by default or
  otherwise.
- Anything that favours one agent vendor over another.
