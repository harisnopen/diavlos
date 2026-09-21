# Publishing

Where Diavlos goes and how it gets there. Three package registries and a
tap. All the names are reserved by publishing, not by asking, so the first
publish is what claims them.

Everything here is ready to run. The only thing missing is a token, and a
token is the one thing that has to come from a person.

## Order

The three crates depend on each other, so they go up in this order and each
one has to appear in the crates.io index before the next can build:

```
diavlos-core  →  diavlos-client  →  diavlos
```

Then npm, then the tap. The tap needs a release to exist first, because it
points at the release's binaries.

## 1. crates.io

**Names:** `diavlos`, `diavlos-core`, `diavlos-client`. All three were free
as of 2026-09-21. Publishing claims them.

Get a token once, at https://crates.io/settings/tokens. Scope it to
"publish-new" and "publish-update". Then:

```bash
export CARGO_REGISTRY_TOKEN=<token>

cargo publish -p diavlos-core
# wait for the index to carry it, usually under a minute
cargo publish -p diavlos-client
cargo publish -p diavlos
```

Check first without uploading anything:

```bash
cargo publish --dry-run -p diavlos-core
```

The dry run for `diavlos-client` and `diavlos` will fail until the crate
below them is really on crates.io. That is expected; it is not a problem
with the package.

Each crate carries its own README, a homepage of https://diavlos.sh, and
keywords. `docs.rs` builds the API docs by itself once a crate lands.

**A published version cannot be deleted**, only yanked. Get the version
right before the first push.

## 2. npm

**Name:** `diavlos`. Claimed 2026-09-21, published from the account
`charisn`.

The package is a shim: it downloads the right signed binary for the
platform on install. It ships three small files and no binary of its own.

**Nobody publishes this by hand any more.** `.github/workflows/npm.yml`
runs when a GitHub release is published and publishes through npm's
*trusted publishing*: GitHub hands npm a short-lived OIDC token proving
that this workflow, in this repository, is doing the publishing. There is
no `NODE_AUTH_TOKEN`, no stored secret, and nothing that can leak. npm
attaches a provenance attestation on the way through.

It runs on `release: published` rather than on the tag, because the shim
downloads the binary from that release. Before publishing it checks three
things and refuses on any of them:

- the tag starts with `v`, and `packaging/npm/package.json` carries exactly
  that version — a shim that says 1.0.1 downloads the 1.0.1 release, so a
  mismatch would ship something that cannot install;
- all five archives are really on the release;
- the version is not already on npm (so a re-run is a no-op, not a failure).

`workflow_dispatch` takes a tag if it ever needs running by hand.

**The one-time setup**, on npm, at
https://www.npmjs.com/package/diavlos/access → Trusted Publisher:

| Field | Value |
|---|---|
| Publisher | GitHub Actions |
| Organization or user | `harisnopen` |
| Repository | `diavlos` |
| Workflow filename | `npm.yml` |
| Environment | *(blank)* |

If the workflow is ever renamed, that field has to change with it or the
publish stops working.

Trusted publishing needs npm CLI 11.5.1 or newer and Node 22.14 or newer;
the workflow installs `npm@latest` to be sure.

The shim downloads from the GitHub release for its own version, so **the
release must exist first** or `npm install diavlos` fails at the postinstall
step.

## 3. Homebrew tap

Homebrew only takes third-party formulas from a repo named
`homebrew-<something>`. The tap is live at
https://github.com/harisnopen/homebrew-tap, and a user installs with:

```bash
brew tap harisnopen/tap
brew install diavlos          # once a release exists
brew install --HEAD diavlos   # works today, builds from main
```

The formula is not written by hand. **The release workflow generates it**
with the real checksums of the binaries it just built and signed, and
attaches it to the release as `diavlos.rb`. The tap then picks it up by
itself: `.github/workflows/sync.yml` runs hourly, downloads
`releases/latest/download/diavlos.rb`, checks it parses as Ruby and defines
`class Diavlos < Formula`, and commits it only if it differs from what is
there. Nothing to do by hand on a release.

To pull a version in right now instead of waiting for the hour, run the
**sync formula** workflow from the tap's Actions tab.

`packaging/homebrew/diavlos.rb` in this repo is a different thing: a
build-from-source formula for installing a tag with no release. The tap does
not use it.

## 4. The skill

There is no registry you push a skill to. Publishing one means hosting it in
a public repo and getting it listed. That is already done: `skills/diavlos/`
holds a spec-conformant `SKILL.md`, and the repo also carries the two
manifests that make it a Claude Code plugin.

**Installing it, today, with nothing published:**

```bash
# Any of ~45 agent products that read the Agent Skills format
npx skills add harisnopen/diavlos

# Or copy it where your agent looks
cp -r skills/diavlos ~/.claude/skills/      # Claude Code, claude.ai
cp -r skills/diavlos ~/.agents/skills/      # Codex
```

**As a Claude Code plugin**, which also brings the eight MCP tools:

```
/plugin marketplace add harisnopen/diavlos
/plugin install diavlos@diavlos
```

**Getting it into the reviewed catalog.** The one path with a real review is
Anthropic's community marketplace. Submit at
https://platform.claude.com/plugins/submit (the Console form works for an
individual; the claude.ai form needs a Team or Enterprise org). Before
submitting:

```bash
claude plugin validate . --strict
```

Pull requests against `anthropics/claude-plugins-community` are closed
automatically; everything goes through the form. After approval the catalog
syncs nightly, so it does not appear at once.

**Listings that are just a pull request**, cheap and worth doing:

- https://github.com/travisvn/awesome-claude-skills
- https://github.com/ComposioHQ/awesome-claude-skills

**skills.sh** indexes by install telemetry rather than submission, so it
picks the skill up once people install it with `npx skills add`. There is no
form.

Keep `SKILL.md` to the six fields the spec allows: `name`, `description`,
`license`, `compatibility`, `metadata`, `allowed-tools`. Claude Code accepts
extra fields, but any of them makes the skill fail validation everywhere
else, including the submission pipeline.

## 5. The website

https://diavlos.sh is a separate deployment. The docs site built from this
repo (`site/`, plus `llms.txt` and `llms-full.txt`) publishes to GitHub
Pages from `.github/workflows/site.yml`.

## What needs a person

| Step | Why |
|---|---|
| crates.io token | Only an account owner can mint one. |
| Naming the trusted publisher on npm | Only a package owner can set it, and only in the browser. Once. |
| Making the release tag | Tag deletion and creation are account actions. |

Everything else in this file is scripted or generated.

## Checklist for a release

1. Bump the version everywhere: `Cargo.toml` (workspace), `packaging/npm/package.json`,
   `bindings/node/package.json`, `bindings/python/pyproject.toml`.
2. `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo test --workspace`.
3. Merge to `main`.
4. Create the tag on `main`. The release workflow builds five platforms,
   signs each with sigstore, writes the SBOM, generates `diavlos.rb`, and
   publishes the release.
5. `cargo publish` the three crates, in order.
6. The npm workflow publishes the shim by itself, once the release is out.
7. The tap syncs its formula within the hour, by itself.
8. Check the three ways in actually work:
   ```bash
   curl -fsSL https://raw.githubusercontent.com/harisnopen/diavlos/main/install.sh | sh
   npx diavlos --version
   brew tap harisnopen/tap && brew install diavlos && diavlos --version
   ```
