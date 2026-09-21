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

**Name:** `diavlos`. Free as of 2026-09-21.

The package is a shim: it downloads the right signed binary for the
platform on install. It ships three small files and no binary of its own.

```bash
cd packaging/npm
npm pack --dry-run          # see exactly what goes up
npm login
npm publish --access public
```

The shim downloads from the GitHub release for its own version, so **the
release must exist first** or `npm install diavlos` fails at the postinstall
step.

## 3. Homebrew tap

Homebrew only takes third-party formulas from a repo named
`homebrew-<something>`. So:

- Repo: `harisnopen/homebrew-tap`
- A user then runs `brew tap harisnopen/tap && brew install diavlos`

The formula is not written by hand. **The release workflow generates it**
with the real checksums of the binaries it just built and signed, and
attaches it to the release as `diavlos.rb`. Publishing a new version to the
tap is therefore a copy:

```bash
TAG=v1.0.0
curl -fsSLO "https://github.com/harisnopen/diavlos/releases/download/$TAG/diavlos.rb"
# in a clone of harisnopen/homebrew-tap
mkdir -p Formula && mv diavlos.rb Formula/diavlos.rb
git commit -am "diavlos $TAG" && git push
```

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
| npm login | Same. |
| Creating `homebrew-tap` | A new public repo under the account. |
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
6. `npm publish` the shim.
7. Copy `diavlos.rb` from the release into the tap.
8. Check the three ways in actually work:
   ```bash
   curl -fsSL https://raw.githubusercontent.com/harisnopen/diavlos/main/install.sh | sh
   npx diavlos --version
   brew tap harisnopen/tap && brew install diavlos && diavlos --version
   ```
