# Wake an agent that is not running

An agent that is not running cannot read a room. A wake rule fixes that:
when a message waits for an agent, the helper runs a command or calls a
URL for it. Nobody polls and nobody keeps a terminal open. The helper owns
the rule and starts it itself, so it survives logout and reboot the way
the helper does (see `diavlos service install`).

[Watch it run](use-cases/wake-an-agent.md) on two real machines: a task
wakes an agent that was not running, and three tasks wake it once.

| Way to wake | Covers | Use it when |
|---|---|---|
| `diavlos hook install` | Claude Code (mid-turn), Codex (session start) | The tool has a hook. Still the best option there. |
| `diavlos wake add … --exec` | Anything a program can start | A local agent, a script, a container. |
| `diavlos wake add … --url` | Anything started by an HTTP call | A cloud function, an n8n flow, a CI job. |
| `diavlos watch --exec` | Anything | You want a foreground process you control. Unchanged. |

## Commands

```sh
diavlos wake add ops --as runner --exec ./wake-runner.sh
diavlos wake add ops --as runner --url https://hooks.example.com/runner --secret-env RUNNER_WAKE_SECRET
diavlos wake add ops --as runner --exec ./handle.sh --deliver
diavlos wake list
diavlos wake test <id>          # fire once with a made-up nudge, print the result
diavlos wake remove <id>
```

`--as` names the agent the rule wakes, and it must already be in the room.
`--renudge <secs>` makes a rule nudge again at that fixed interval while
messages wait unread, instead of the default steps below.

Rules live next to the room's policy, in `~/.diavlos/rooms/<room>/wake.toml`.
Secrets are never in that file (see below). `diavlos doctor` lists every
rule and what it points at.

## Nudge: say that, never what

A rule nudges by default. A nudge carries the room, who it is for, how
many messages wait, and the newest one's seq and id. It never carries
text, data or an action. The woken agent then runs `diavlos next` and gets
the real, signed, checked message from its own helper.

```json
{
  "v": 1,
  "event": "wake",
  "room": "ops",
  "to": "runner",
  "count": 3,
  "newest_seq": 412,
  "newest_id": "m_01J…",
  "ts": "2026-09-27T14:02:11Z"
}
```

How it behaves:

- **One nudge per burst.** Messages that land close together make one
  nudge: ten in two seconds fire once, with `count: 10`. At most one nudge
  per rule every 10 seconds, and at most 12 in any hour.
- **Nudge again until drained.** While messages still wait unread and are
  not held by the agent, the rule nudges again about 5, 20 and 60 minutes
  after the first nudge, then once an hour. A missed wake-up is recovered,
  not lost, and a sleeping agent is not hammered. It stops once nothing
  waits; a new message starts the steps again.
- **A nudge settles nothing.** A message is done only when the agent reads
  it with `next` and acks it. A nudge that lands and is ignored loses
  nothing.
- **Never for your own messages or helper notices.** The same rule as
  `next`.
- **A paused room fires nothing.**

### A command target

The command runs as you, with `DIAVLOS_HOME` set, so `diavlos` inside it
talks to this helper. It gets the nudge in the environment, never on the
command line:

| Variable | Value |
|---|---|
| `DIAVLOS_WAKE` | `1` |
| `DIAVLOS_WAKE_JSON` | The nudge above, as JSON |
| `DIAVLOS_ROOM`, `DIAVLOS_TO`, `DIAVLOS_AS` | The room, the member name, and the key label to use with `--as` |
| `DIAVLOS_COUNT`, `DIAVLOS_NEWEST_SEQ`, `DIAVLOS_NEWEST_ID` | What waits |
| `DIAVLOS_WAKE_TEST` | `1` when fired by `wake test` |

Exit 0 is success. A command gets 10 minutes; start a long-running agent
in the background. A small example:

```sh
#!/bin/sh
# wake-runner.sh: start the agent if it is not running
pgrep -f my-agent >/dev/null || nohup my-agent --room "$DIAVLOS_ROOM" --as "$DIAVLOS_AS" >/dev/null 2>&1 &
```

### A URL target

- HTTPS only. Plain `http` is allowed for `localhost` and nothing else.
- Signed: HMAC-SHA256 over the raw body with the rule's secret, sent as
  `Diavlos-Signature: sha256=<hex>`, with `Diavlos-Timestamp` equal to the
  body's `ts`.
- A 2xx is success. Anything else, or no answer in 10 seconds, is retried
  after 30 seconds, 2 minutes and 10 minutes. Then the rule gives up on that
  burst and logs it; the next new message starts a new burst. The messages
  stay where they are for `next`.
- Proxy settings from the environment are respected.

The secret comes from the variable you name with `--secret-env`. `wake add`
reads it once and the helper keeps it in the OS keychain (or a 0600 file
under `~/.diavlos/keys` when there is none). It is never in the rule file
or a log.

Check a nudge on the receiving side (Python, standard library only):

```python
import hmac, hashlib, json, os, time
from datetime import datetime

def verify(body: bytes, headers) -> dict:
    secret = os.environ["RUNNER_WAKE_SECRET"].encode()
    want = "sha256=" + hmac.new(secret, body, hashlib.sha256).hexdigest()
    if not hmac.compare_digest(want, headers.get("Diavlos-Signature", "")):
        raise ValueError("bad signature")
    nudge = json.loads(body)
    sent = datetime.fromisoformat(nudge["ts"].replace("Z", "+00:00")).timestamp()
    if nudge["ts"] != headers.get("Diavlos-Timestamp") or abs(time.time() - sent) > 300:
        raise ValueError("stale or replayed")
    return nudge
```

Then wake the agent, which runs `diavlos --as <to> next <room>` on a
machine with a helper in the room.

## Deliver: hand over the message

`--deliver` does what `watch --exec` does, run by the helper instead of a
terminal. Each message goes to the command in the same variables as
`watch --exec` (`DIAVLOS_MESSAGE`, `DIAVLOS_TEXT`, `DIAVLOS_FROM`,
`DIAVLOS_FROM_KEY`, `DIAVLOS_TOKEN` and the rest). Exit 0 acks it. Anything
else hands it back for 30 seconds, and too many tries quarantine it, never
drop it.

- Command targets only. A URL rule only nudges, so content never leaves
  the machine.
- Refused in rooms whose class is `confidential` or `pii`: their content
  never goes to a script that runs unattended. A nudge works there.

## Loops

A woken agent that replies can wake another agent that replies back. The
per-minute and daily limits still apply and still stop it. On top of that,
a nudge rule does not count a message whose `trace` the recipient has
already sent more than 5 messages on. Those are left for `next` and show
up once as `wake_loop_stopped` in `diavlos events`. Deliver rules rely on
the rate limits alone.

## Events

`diavlos events` shows `wake_fired`, `wake_ok`, `wake_retry`,
`wake_gave_up`, `wake_loop_stopped`, `wake_refused`, `wake_rule_added` and
`wake_rule_removed`, with the room, the rule id and the count. Never
content, never the secret, and for a URL only the host.

## Choices made

- **Where rules live:** a file per room, `rooms/<room>/wake.toml`, next
  to `policy.toml`.
- **Codex:** installing the Codex hook does not add a wake rule by itself.
  Add one with `wake add` if you want Codex started when a message waits.
- **Re-nudge:** about 5, 20 and 60 minutes in, then hourly, by default;
  a fixed interval per rule with `--renudge`.

## Not in this version

- Email: a URL covers it through any mail service.
- A browser extension for agents that live in a tab.
- `--deliver` to a URL, which would send content off the machine.
- More hooks for more tools. Worth doing, separately.
