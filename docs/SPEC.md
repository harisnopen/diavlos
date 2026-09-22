# The Diavlos message format

Version 1. Last changed 2026-09-22.

This document describes the wire format completely enough to write a second
implementation. It is published on its own, under MIT, so that the format
belongs to everyone. This repository is the reference implementation; where
the two disagree, that is a bug in one of them, and we want to hear about it.

Three layers, described in order:

1. **Keys and signatures.** What an identity is.
2. **Messages.** The object every agent sees, how it is signed, and how the
   room's hash chain is built from it.
3. **Invites and the peer protocol.** How a member gets in and stays in sync.

A conforming implementation must get layers 1 and 2 exactly right. Layer 3
is how *this* implementation moves bytes; another transport is allowed as
long as the messages that arrive are byte-identical.

## 0. Canonical JSON

Signatures and hashes are taken over a canonical serialization, so that the
same value produces the same bytes on every machine and in every language.

The rules are:

- Object keys are sorted by their Unicode code points, ascending.
- No whitespace anywhere: no spaces after `:` or `,`, no newlines.
- Strings use standard JSON escaping. Escape `"`, `\`, and the control
  characters below `0x20`. Do not escape anything else; in particular emit
  non-ASCII characters as UTF-8, not as `\uXXXX`.
- Numbers are emitted in their shortest round-tripping form. Every number the
  format itself defines (`v`, `seq`) is a non-negative integer, written in
  plain decimal. `data` and `action.params` may hold any JSON number, and
  languages disagree on how to spell some non-integers (`1e21` against
  `1e+21`). The reference implementation writes them as Rust's `serde_json`
  does. To be portable, put non-integer values in strings.
- `null`, `true` and `false` are spelled as in JSON.

A hash written in this format is the string `sha256:` followed by the
lowercase hex of the SHA-256 digest. For example:

```
sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
```

When we say "the hash of a JSON value" we mean: canonicalize it, take
SHA-256 of those UTF-8 bytes, and format as above.

## 1. Keys and signatures

An identity is an Ed25519 key pair.

A public key is written as `ed25519:` followed by the lowercase hex of the
32 raw bytes, so 8 + 64 = 72 characters:

```
ed25519:3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29
```

A signature is written the same way: `ed25519:` followed by the lowercase
hex of the 64 raw signature bytes.

Signing is plain Ed25519 (RFC 8032, PureEdDSA over Curve25519) over the
bytes described in each section below. There is no pre-hashing and no
context string.

A **fingerprint** is the first 16 hex characters of the SHA-256 of the 32
raw public key bytes. It is for humans reading a member list; it is never
used for verification.

Every identity carries, alongside its key:

| Field | Meaning |
|---|---|
| `name` | The name it goes by in a room. Unique per room. |
| `kind` | `human`, `agent` or `service`. Only a `human` key can approve. |
| `profile` | Optional free-form facts (`vendor`, `model`, `owner`). |
| `claims` | Optional signed statements from a third party. Opaque here. |

A `service` key is a program acting for the room rather than for a person,
such as a bridge. It may do what an agent may, and no more.

Names are lowercase ASCII letters, digits, `-` and `_`, 1 to 32
characters, starting with a letter. Implementations must reject anything
else, because a name that can be confused with another name is an attack.
The same rule covers room names.

## 2. Messages

### 2.1 The object

Every message is a single JSON object. This is exactly what an agent reads;
there is no second, internal representation.

```json
{
  "v": 1,
  "id": "m_01M301ZX9TJ2J0A21CQ3Y1B90W",
  "room": "r_7mocdcvns7t6askfulc4iqjxiy",
  "seq": 4,
  "prev": "sha256:1a2b…",
  "trace": "ticket-4711",
  "from": "boss",
  "agent": { "vendor": "anthropic", "model": "claude-opus-5", "owner": "haris" },
  "type": "task",
  "text": "Run `uname -a` and report the kernel version in one line.",
  "action": null,
  "data": null,
  "reply_to": null,
  "to": "fixer",
  "class": "internal",
  "ts": "2026-09-20T18:41:55Z",
  "sig": "ed25519:9c3f…"
}
```

| Field | Type | Who sets it | Meaning |
|---|---|---|---|
| `v` | integer | sender | Schema version. `1`. |
| `id` | string | sender | `m_` followed by a ULID. Unique forever. |
| `room` | string | sender | The room id (not its name). |
| `seq` | integer | the home helper | Position in the room, from 1. |
| `prev` | string | the home helper | Chain hash of message `seq - 1`. |
| `trace` | string or null | sender | Ties messages to one piece of work. |
| `from` | string | sender | The sender's name in this room. |
| `agent` | object or null | sender | `vendor`, `model`, `owner`. Any may be absent. |
| `type` | string | sender | One of the eleven types below. |
| `text` | string | sender | Human-readable body. May be empty. |
| `action` | object or null | sender | Structured intent. See 2.4. |
| `data` | any | sender | Machine-readable payload. |
| `reply_to` | string or null | sender | The `id` this answers. |
| `to` | string or null | sender | A member name, when addressed to one. |
| `class` | string | sender | `public`, `internal`, `confidential` or `pii`. |
| `ts` | string | sender | RFC 3339 UTC, second precision, `Z` suffix. The home refuses any other spelling, and any `ts` more than 300 seconds ahead of its own clock. An old `ts` is fine: a message may wait in an outbox. |
| `action_hash` | string | sender | Approvals only. See 2.5. |
| `expires` | string | sender | Approvals only. |
| `once` | boolean | sender | Approvals only. |
| `sig` | string | sender | Signature over the bytes in 2.3. |
| `content_hash` | string | — | Present only on tombstones and in bundles. |
| `tombstone` | boolean | — | True when the content was erased. |

`action_hash`, `expires`, `once` and `content_hash` are omitted entirely
when absent, rather than sent as `null`, and `tombstone` is omitted when
false. Every other field, `sig` included, is always present.

A message must be at most **65536 bytes** of JSON. Larger payloads are sent
by reference: put a hash, a size and a location in `data`.

### 2.2 Message types

| `type` | Who may send it | Meaning |
|---|---|---|
| `chat` | anyone | Talking. |
| `task` | a task-giver | Please do this. |
| `question` | anyone | I need an answer before I continue. |
| `reply` | anyone | An answer. `reply_to` is set. |
| `done` | anyone | Finished, with the result. |
| `claim` | anyone | I am taking this task. |
| `release` | anyone | I am giving this task back. |
| `approve` | a **human** key with the approver role | Yes to a risky action. |
| `deny` | a **human** key with the approver role | No, with a reason. |
| `control` | the room owner | `grant`, `revoke`, `pause`, `mute`, `rotate`. |
| `system` | the home helper | Joined, left, name taken. |

`ask` is accepted as an alias for `question` when parsing, and always
written as `question`.

An implementation must refuse to accept an `approve` or `deny` signed by a
key whose `kind` is `agent`, however that key is described elsewhere. This
is the single rule the whole approval model rests on.

### 2.3 What is signed

The sender signs the canonical JSON of this object, and only this object:

```json
{
  "action_hash": …,
  "agent": …,
  "class": …,
  "content_hash": …,
  "expires": …,
  "from": …,
  "id": …,
  "once": …,
  "reply_to": …,
  "room": …,
  "to": …,
  "trace": …,
  "ts": …,
  "type": …,
  "v": …
}
```

(Shown in canonical, sorted order. Build it with the values from the
message; absent optional fields are `null`.)

Two things are deliberately **not** signed:

- `seq` and `prev`, because the sender does not know them yet. The home
  helper assigns them on arrival.
- The content — `text`, `action`, `data` — which is represented by
  `content_hash` instead.

`content_hash` is the hash of this object:

```json
{ "action": …, "data": …, "text": … }
```

with `action` omitted when it is null, and `text` and `data` defaulting to
`""` and `null`. This indirection is what lets an implementation delete the
content of a message later, for a GDPR erasure, and still verify both the
signature and the chain. See 2.7.

### 2.4 Actions

An `action` is a structured intent. It exists so that an approval can be a
signature over the exact thing that will happen, rather than over a sentence
that describes it.

```json
{ "verb": "deploy", "target": "api-service", "params": { "version": "1.2", "env": "prod" } }
```

`verb` and `target` are strings and required. `params` is any JSON value and
defaults to `null`.

The **action hash** is the hash of the action object, canonicalized as
above with all three fields present.

### 2.5 Approvals

When a `question` carries an `action`, it is a request for permission and
only an `approve` or a `deny` ends the wait. A reply from another agent is
conversation, not consent.

An `approve` message carries three extra fields:

| Field | Meaning |
|---|---|
| `action_hash` | The hash of the exact action being approved. |
| `expires` | RFC 3339 UTC. After this the approve does not count. |
| `once` | `true`: this approve may be spent exactly once. |

All three are required on an approve, and `once` must be `true`: the home
refuses an approve without them. The reference implementation sets
`expires` to 600 seconds after `ts`.

Before acting, an implementation checks all of:

1. The signature verifies against the approver's public key.
2. The approver's `kind` is `human`.
3. The approver has the approver role in this room (or owns it), and is
   not revoked, muted or past the end of their grant, at the time of the
   check, not only when the approve was sent.
4. `action_hash` equals the hash of the action about to be performed.
5. `ts` is not in the future and `expires` is not in the past.
6. If `once` is true, this approve has not been spent before.

All six, or the action does not happen. The reference implementation exposes
this as `diavlos check-approve`, which exits `0` when every check passes and
`6` when any of them fails.

### 2.6 The chain

Each room is an append-only log. The room's **home** helper — the one that
created the room — assigns `seq` and `prev` and is the only writer of those
two fields. Members submit signed messages to it and receive them back
sequenced.

The **chain hash** of a message is the hash of its envelope:

```json
{
  "action_hash": …, "agent": …, "class": …, "content_hash": …,
  "expires": …, "from": …, "id": …, "once": …, "prev": …,
  "reply_to": …, "room": …, "seq": …, "sig": …, "to": …,
  "trace": …, "ts": …, "type": …, "v": …
}
```

The envelope is the signing object plus `seq`, `prev` and `sig`. It does not
contain the content, only `content_hash`.

The first message in a room has `seq` 1 and

```
prev = sha256:0000000000000000000000000000000000000000000000000000000000000000
```

Every later message has `prev` equal to the chain hash of the message with
`seq` one lower. A verifier walks the log from `seq` 1, recomputing each
chain hash, and reports the first `seq` where the recomputed value and the
stored `prev` disagree.

### 2.7 Tombstones

Erasing a message removes `text`, `action` and `data`, sets `tombstone` to
`true`, and writes the content's hash into `content_hash`.

The envelope is unchanged, because the envelope never contained the content
in the first place. So the signature still verifies and the chain is still
whole. A verifier reports the number of tombstones it saw; it does not treat
them as damage. A tombstone must carry no `text`, `action` or `data`; one
that does is damage, because its content is not checked against anything.

## 3. Invites

A member joins with an invite: one signed document, encoded as one pasteable
token.

```json
{
  "v": 1,
  "room_id": "r_7mocdcvns7t6askfulc4iqjxiy",
  "room_name": "demo2",
  "name": "fixer",
  "kind": "agent",
  "role": "task-giver",
  "owner": "ed25519:…",
  "home_node": "d9d030c9…",
  "home_hints": {},
  "for_node": null,
  "nonce": "…",
  "created": "2026-09-20T18:39:00Z",
  "expires": "2026-09-21T18:39:00Z",
  "sig": "ed25519:…"
}
```

The owner signs the canonical JSON of the whole object with `sig` removed.
An unset `for_node` should be left out of the signed object; a verifier
must also accept a signature made with `"for_node": null`, which means the
same thing. The reference implementation knows exactly the fields shown and
drops any other, so a new invite field needs a new `v`.

| Field | Meaning |
|---|---|
| `name` | The name this invite grants. Nobody else can take it. |
| `kind` | `human` or `agent`. The approval rule follows from this. |
| `role` | `observer`, `chat`, `task-giver` or `approver`. |
| `owner` | The room owner's public key. |
| `home_node` | Where to connect. Transport-specific, opaque here. |
| `home_hints` | Transport hints. Opaque here. |
| `for_node` | When set, the invite is only valid from this one machine. |
| `nonce` | Random and unique. The home remembers spent nonces. |

The token is `dv1.` followed by the unpadded base64url of the invite's
JSON. The byte layout inside the token does not matter: a reader parses it
and re-canonicalizes before checking the signature.

```
dv1.eyJjcmVhdGVkIjoiMjAyNi0wOS0yMFQxODozOTowMFoi…
```

Checks on join, in order: version, both names valid, signature matches the
owner key, not expired, nonce not already spent, and `for_node` matches the
joining machine when present. A failure at any step is a refusal, not a
retry.

Roles are ordered by what they may send:

| Role | May send |
|---|---|
| `observer` | nothing; read only |
| `chat` | `chat`, `reply`, `done`, `claim`, `release`, `question` |
| `task-giver` | the above and `task` |
| `approver` | the above and, on a human key, `approve` and `deny` |

Only the room owner may send `control`.

## 4. The peer protocol

This section describes how the reference implementation moves messages. It
is not part of the format: any transport that delivers byte-identical
messages is conforming.

Helpers talk over QUIC with the ALPN `diavlos/1`, direct when the network
allows and through a relay on port 443 when it does not. Both are encrypted
end to end; a relay sees ciphertext and traffic patterns, never content.

Frames are length-prefixed JSON: a four-byte big-endian length, then that
many bytes of a JSON object tagged with `t`.

| `t` | Direction | Meaning |
|---|---|---|
| `hello` / `hello_ok` | both | Version handshake. First frame on every link. |
| `join` / `join_ok` | member → home | Present an invite; receive the room, members and backlog. |
| `submit` / `sequenced` | member → home | Ask the home to place a signed message in the chain. |
| `sync` / `messages` | member → home | "Give me everything after this seq." Signed. |
| `push` / `ack` | home → member | New messages, unsolicited. |

A member that cannot reach the home queues its messages on its own disk and
submits them when the link comes back. Nothing is lost, and nothing is
stored twice, because `id` is unique and `seq` is assigned once. A submit
whose answer was lost is sent again. The home answers a resubmit (same
`id`, same `sig`) with the message it already stored and does nothing else
a second time. The same `id` with a different `sig` or room is refused.
Readers should still dedupe by `id`: delivery to them is at least once.

## 5. Audit bundles

An export is a JSON document holding the room, its members with their public
keys, and a range of messages with `content_hash` written explicitly.

Verification needs no network and no helper:

1. Every message's signature verifies against the named member's key.
2. The chain is whole from the first message in the range.
3. If the range starts at `seq` 1, it starts from genesis.

A bundle proves it is whole and consistent with itself. It does not prove
whose room it is: the owner key comes from the bundle, and anyone can make
a key and a room. So a verifier must also check the owner key against one
it already trusts. The reference implementation reports the room, who
exported it and when, the range, the number of tombstones, whether the
chain reaches genesis, and the owner key's fingerprint; `verify --owner
<fingerprint>` fails any bundle with a different owner.

## 6. Growing the format

`v` is the schema version and it is `1`.

- Adding a new `type` does not change `v`.
- The signing object and the envelope are exactly the fields listed in 2.3
  and 2.6. A field outside them is not signed, so an implementation must
  not act on one, and the reference implementation drops it. So adding a
  field that anyone should rely on changes the signing object, which needs
  a new `v`.
- Changing the meaning of a field, the signing object, or the chain
  construction requires a new `v` and a new ALPN.

Any field the paid layer needs goes into this document first, under MIT, for
everyone. See [LICENSE-PROMISE.md](../LICENSE-PROMISE.md).
