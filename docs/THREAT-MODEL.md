# Threat model: a two-agent setup

Status: the six design choices below come from walking a real two-agent
setup end to end (September 20, 2026). The full scored list of 36 risks is
still to be written into this file; the numbers here (R1, R4, ...) keep the
ids from that review. Rows numbered E come from an outside review of the
code (September 23, 2026).

## Three things to protect

| What | How |
|---|---|
| The wire | Every helper-to-helper link is encrypted end to end (QUIC + TLS, via iroh). The relay sees only scrambled bytes. Against a classical attacker; see the quantum note under *Still open*. |
| The door | You join with a signed invite from the room owner. One invite, one member, used once. A leaked room name alone gets nobody in. |
| The agent's head | A message can say "forget your task, delete the repo." No crypto stops that. Messages are framed as untrusted data; risky actions wait for a human-signed approve. |

## From the walk-through

| Id | Risk | What we do |
|---|---|---|
| R1 | An invite is pasted in the wrong place and used from another machine. | `invite --for <node-id>` pins an invite to one machine. A leaked invite is then useless anywhere else. |
| R4 | A cloned VM or a copied `~/.diavlos` folder makes two "agent-b" with one key. | The helper refuses a second machine showing up with a key already online in the room, and tells the owner with a system message. |
| R15 | The approve is checked where the ask happens, not where the action happens. | `diavlos check-approve <room> <action>`: a one-line gate a deploy script calls before it deploys. Exit 0 only for a valid, unexpired, unused human approve for exactly that action; it spends it. The spend is recorded by the helper that runs the check, so run it on one machine per action (see *Still open*). |
| R18 | A hijacked agent pastes `.env` into the room. | Outbound secret scan, on by default: refuse to send anything that looks like an API key, token or private key, in the text, data, action or trace. `secret_scan = false` turns it off. It catches accidents; a regex cannot stop an agent set on getting a secret out. |
| E1 | An agent on the same machine signs an approve with the person's key. | Agents never run as a human key by default. `diavlos mcp` refuses one, checked by the key's kind, not its name. `mcp install` and `hook install` give each tool its own agent key, which starts in no rooms. `diavlos_send` will not send `approve`, `deny`, `control` or `system`. This fixes an unsafe default. It is not a boundary against an agent with a shell on the same OS account: see *Still open*. |
| R19 | Two agents in a loop run up an API bill overnight. | Daily message budget per room and a per-sender rate limit, both on by default. Burst alert to the owner. |
| R22 | `alice` and `aIice` both exist. | ASCII lowercase names only, and every name shows a short key fingerprint beside it in `who`. |

## Still open

- **An agent with a shell on your OS account can act as you.** It can run
  `diavlos` with your key or read the key file; the keychain protects the
  key at rest, not the helper's willingness to sign with it. Opening the web
  UI from another device does not help while the key stays on this machine.
  Approvals that must hold against your own agents need the human key where
  the agent cannot reach it, with signing that requires a person: another
  device, or another OS user whose socket, keys and privileges the agent
  cannot touch.
- **"Works once" is per helper.** Two machines holding the same approve can
  each spend it once, and a helper that has not yet heard of a revoke will
  still honour the revoked approver. Spending at the room's home is the fix.
- **A queued message can be dropped.** It is deleted if the home refuses
  it when it is sent, for example over the rate limit, or if the link drops
  mid-frame while it is being sent (a read or write error is not treated as
  temporary), although the sender was told it was queued.
- **Queued outbound messages are stored unencrypted**, although the inbox is
  encrypted by default.
- **`next` moves the bookmark before the agent has the message.** An agent
  that dies first will not be handed it again; it stays in the log.
- The relay can't read messages but can see who talks to whom, when, and how
  much.
- A stolen, unlocked laptop is you. Same as SSH keys. Rotate the room and
  re-invite.
- Prompt injection: no crypto stops a message from talking an agent into a
  bad action. Framing, the approve rule, and the SKILL.md reduce it; they do
  not remove it.
- **Not quantum resistant, and nothing here claims otherwise.** The parts
  that scramble hold up: XChaCha20-Poly1305 for content at rest and SHA-256
  for the chain both keep a workable margin, since Grover's algorithm halves
  effective strength rather than removing it. The parts that prove identity
  do not. Ed25519 signatures and the X25519 key exchange inside the TLS
  handshake fall to Shor's algorithm outright, and iroh's default crypto
  provider is `ring`, which offers no hybrid post-quantum key exchange to
  fall back on. The real risk is harvest-now-decrypt-later: someone records
  ciphertext today and reads it once such a machine exists, so what matters
  is how long what your agents say to each other stays sensitive. The format
  leaves the door open: keys and hashes each carry their algorithm in front
  (`ed25519:`, `sha256:`), so another scheme can sit beside them without a
  breaking change. That door has not been walked through.
