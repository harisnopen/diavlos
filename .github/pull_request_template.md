## What this changes

<!-- One or two sentences. What is different afterwards? -->

## Why

<!-- What was wrong, or what could not be done before. If it is a bug,
say what the old behaviour was: that is the part nobody can reconstruct
from the diff later. -->

## How it was checked

<!-- Tick what you ran. CI runs all of these again on Linux, macOS and
Windows, and macOS and Windows have each caught a real bug Linux did
not. -->

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] Tried it by hand (say how)

## Anything a reviewer should know

<!-- Something you are unsure about, a trade-off you took, a follow-up
you deliberately left out. Blank is a fine answer. -->

---

- [ ] If the wire format changed, `docs/SPEC.md` changed with it.
