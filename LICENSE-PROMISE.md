# The promise

Diavlos is open source because a channel nobody can inspect is a channel
nobody should trust. That only works if you can count on it staying open.
So here is what we will and will not do, in writing.

## What is MIT stays MIT

Every line in this repository is MIT, today and forever. That covers:

- `crates/core`: keys, signed messages, invites, rooms, bundles, the inbox,
  the wire format.
- `crates/client`: the library.
- `crates/cli`: the `diavlos` binary. The helper, every command, the MCP
  server, the hooks, the web UI, the bridges.
- `bindings/`: the Python and Node bindings.
- `skills/`, `packaging/`, `docs/`: the skill, the installers, the docs.

We will never move any of it behind a paywall. Not in a later version, not
under a new company, not after an acquisition. A release that is MIT cannot
be un-MIT'd, and we will not try.

## New free features stay free

The things that make Diavlos useful on your own machines stay free:
rooms, invites, roles, signed messages, the hash chain, approvals and
`check-approve`, export and verify, MCP, the CLI, hooks, the library.

When we add to that list, the addition is MIT too. We do not build a free
feature, wait for you to depend on it, and then charge for it.

## What we charge for, in one place

The paid layer is everything that needs a server or a company to run it.
It lives in a separate repository, `diavlos-enterprise`, under Fair Source
(FSL-1.1-MIT): you can read it, run it, change it and self-host it; you
cannot resell it as a competing service; and each version becomes MIT two
years after its release.

The paid list, so it is never a surprise:

- Approvals on a phone: push, passkeys, the approver app.
- Single sign-on, signed claims, SCIM, device policies.
- The policy engine's enterprise half: quorum, groups, time windows, model
  governance.
- The vault: an always-on inbox with retention rules and legal holds.
- A private relay with a node allowlist and region pinning.
- Audit ingest, search, SIEM forwarding, the AI Act pack.
- The admin console and API.
- Us operating any of it for you.

If something is not on that list, it is free.

## A licence never stops a message

Enterprise features check a signed licence file. An expired licence makes
the admin console read-only after a 30-day grace period. It does not stop
rooms, messages, approvals or agents. We will never brick your agents to
collect an invoice.

## Core never depends on the paid layer

The dependency arrow points one way. `diavlos-enterprise` depends on this
repository; this repository does not know it exists. You can delete the
paid layer and everything here still builds and runs.

## One wire format

The paid layer adds signed claims and signed policies. Both are ordinary
signed documents a free helper can verify, or ignore, without breaking. A
room with enterprise features in it still works for free members.

If the paid layer ever needs a new field on the wire, that field goes into
[the spec](docs/SPEC.md) first, MIT, for everyone.

## If we go away

Everything self-hosts from the same containers we run. Exports are signed
and verify with no server. Two years after any release of the paid layer,
that release is MIT. The spec is published, so a second implementation is
a reading exercise, not a reverse-engineering one.
