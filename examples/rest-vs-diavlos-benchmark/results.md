# REST/JSON vs Diavlos, 1000 hand-offs each

Run 2026-09-23 01:43 UTC on Linux-6.18.44-fc-v37-x86_64-with-glibc2.39, Python 3.11.15, one machine, two Diavlos helpers. Sender: langchain_core StructuredTool. Receiver: crewai BaseTool.

## Speed

| transport | total | per hand-off p50 | p99 | receiver check p50 | receiver check p99 |
|---|---|---|---|---|---|
| rest | 0.71 s | 0.69 ms | 1.01 ms | 9.6 us | 31.8 us |
| rest+hmac | 0.72 s | 0.69 ms | 1.13 ms | 18.8 us | 61.9 us |
| diavlos | 5.80 s | 5.63 ms | 9.56 ms | 192.1 us | 347.2 us |

A hand-off is a full round trip: the LangChain tool sends a task, the CrewAI tool
checks it and answers, and the sender has the answer. The receiver check is
the shape check, plus the HMAC or the Ed25519 signature where there is one.

## Attacks and outages (run for real, not assumed)

| test | rest | rest+hmac | diavlos |
|---|---|---|---|
| payload changed on the way | accepted | rejected | rejected |
| stranger claims to be the planner | accepted | rejected | rejected |
| another member claims to be the planner | accepted | accepted | rejected |
| receiver offline, 10 hand-offs sent | 10 of 10 lost | 10 of 10 lost | 0 of 10 lost |

## What the numbers say

- Plain REST is the fastest by far and checks nothing but the shape.
- HMAC stops a changed payload and a stranger, but every holder of the
  shared secret can sign as anyone, so it cannot tell the planner from the crew.
- Diavlos costs more per hand-off: each message is signed, chained,
  written to disk before `send` returns, and routed through two helpers.
  For that it rejects changed and forged messages by key, and loses
  nothing while the receiver is down.
