# REST/JSON hand-offs vs Diavlos typed messages

A LangChain tool hands a task to a CrewAI tool 1,000 times. The same two
tools run over three transports, and then face three attacks and one
outage, each run for real:

| transport | what it is |
|---|---|
| `rest` | A JSON POST to a webhook. The receiver checks the payload's shape. |
| `rest+hmac` | The same, plus an HMAC-SHA256 header with a secret both sides share. |
| `diavlos` | A typed `task` in a room, answered with a `done`. Ed25519-signed by each sender's own key, on disk before `send` returns, and checked again by the receiver in Python. |

```sh
pip install -r requirements.txt   # langchain-core, crewai, cryptography
python bench.py                   # needs `diavlos` on your PATH; about 10 s
python bench.py -n 200            # quicker
```

It writes [results.md](results.md). No model key and no network needed:
everything runs on one machine, with two Diavlos helpers standing in for
two machines.

## Results (one real run)

| transport | 1,000 hand-offs | per hand-off p50 | p99 | receiver check p50 | p99 |
|---|---|---|---|---|---|
| rest | 0.71 s | 0.69 ms | 1.01 ms | 9.6 us | 31.8 us |
| rest+hmac | 0.72 s | 0.69 ms | 1.13 ms | 18.8 us | 61.9 us |
| diavlos | 5.80 s | 5.63 ms | 9.56 ms | 192.1 us | 347.2 us |

| test | rest | rest+hmac | diavlos |
|---|---|---|---|
| payload changed on the way | accepted | rejected | rejected |
| stranger claims to be the planner | accepted | rejected | rejected |
| another member claims to be the planner | accepted | accepted | rejected |
| receiver offline, 10 hand-offs sent | 10 of 10 lost | 10 of 10 lost | 0 of 10 lost |

## Reading it straight

- **REST is about 8 times faster.** Diavlos signs each message, chains it,
  writes it to disk before `send` returns, and routes it through two
  helpers. That costs about 5 ms a hand-off on one machine.
- **5 ms is small next to the work.** One model call takes hundreds of
  milliseconds to many seconds. The transport is rarely the slow part of an
  agent hand-off.
- **HMAC fixes tampering, not identity.** Everyone holding the shared secret
  can sign as anyone. Diavlos gives each member its own key, so a member
  cannot speak as another.
- **A webhook needs the receiver up.** Diavlos keeps the message and
  delivers it when the receiver comes back.

## Keeping it fair

- REST gets its best case: localhost, one keep-alive connection, and
  Nagle's algorithm off on both ends. Without that, Python's HTTP stack
  waits about 40 ms on every reply, which would make REST look 60 times
  slower than it is.
- Both use the same two framework tools (`langchain_core.tools.StructuredTool`
  sending, `crewai.tools.BaseTool` receiving), and the same shape check.
- Round trips are measured one at a time, from the sender's tool call to
  the answer in hand.
- The Diavlos room's flood limit (60 messages a minute per sender) is lifted
  for the run, on the local home only. Keep it on in real rooms.
- Across a real network both transports pay the network's latency on top.
