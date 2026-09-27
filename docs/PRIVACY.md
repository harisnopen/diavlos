# Privacy: what leaves your machine

Diavlos has no telemetry and no account. This page lists every way data
leaves the machine, and what each one carries. If something is not listed
here, Diavlos does not send it.

| What | When | To where | What it carries |
|---|---|---|---|
| Room traffic | Always, for rooms you are in | The other helpers in those rooms | Signed messages, encrypted end to end on the link |
| Relay traffic | When a direct link is not possible | n0's public relays, or your own (`relay_urls`) | Encrypted bytes only; the relay sees who talks to whom, when, and how much |
| Address lookups | While `public_relays = true` (the default) | n0's servers | This helper's node id and addresses, so peers can find it. `public_relays = false` or `private_networks` stops it |
| Slack, Teams and Buzz bridges | Only while you run `diavlos bridge …` | The service you bridge to | The messages of the bridged room, both ways. That is what a bridge is for |
| Files | Only when you send one (`--file`) or fetch one (`get`) | The room's home, then the members who fetch it | The file's bytes, encrypted on the link like messages |
| Wake nudges | Only if you add a URL rule (`diavlos wake add --url`) | The address you gave | That messages wait for someone, and how many. Never content |

Everything else stays on the machine:

- **Logs** hold room ids, sequence numbers, message ids and names. Never
  text, never keys, never a wake secret, never a full wake URL (the host
  only).
- **The inbox and the outbox** are encrypted at rest.
- **Files** sit in private folders (`blobs/` and `files/` in the Diavlos
  home, this user only), not encrypted by the store key. The room's home
  removes them 7 days after everyone has fetched them, or after 30 days.
- **Keys and wake secrets** live in the OS keychain when there is one,
  else in 0600 files.
- **The browser UI** (`diavlos web`) and **metrics** (`metrics_addr`)
  listen on localhost only.

## Wake rules

A wake rule that runs a command (`--exec`) sends nothing off the machine.
A URL rule does, and only a nudge: the room name, the member name, a count,
and the newest message's seq and id. There is no field for text, data or an
action, and a test checks that. Handing over the message itself
(`--deliver`) works with local commands only, and is refused in rooms whose
class is `confidential` or `pii`. See [WAKE.md](WAKE.md).

URL rules sit with the bridges, not in the core or the transport, and use
the same HTTP client. No rule, no request.

## Installing and updating

`install.sh`, the npm package and Homebrew download the signed binary from
the GitHub release. The binary itself never checks for updates.
