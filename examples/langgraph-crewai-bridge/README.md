# LangGraph to CrewAI, over Diavlos

A LangGraph agent hands a job to a CrewAI agent on another machine. Every
message is signed with Ed25519, and each side checks the other's signature
itself. Before anything is published, a human says yes from the command
line, and a gate checks that yes against the exact bytes being published.

```
 machine A                                     machine B
 ┌──────────────────────────┐   Diavlos room  ┌──────────────────────┐
 │ LangGraph planner         │  ── task ──▶   │ CrewAI worker         │
 │  delegate                 │  ◀─ claim ──   │  check signature      │
 │  wait_for_crew  (checks)  │  ◀─ done ───   │  run the crew         │
 │  ask_human ── question ─▶ you, in the CLI  └──────────────────────┘
 │  publish   ◀─ approve ─── (a human key)
 │   └ diavlos check-approve (the gate)
 └──────────────────────────┘
```

No server, no webhook, no shared secret. The two agents only share a room.

## Run it on one machine

```sh
pip install -r requirements.txt          # langgraph, crewai, cryptography
./run_local.sh                           # needs `diavlos` on your PATH
```

Two helpers stand in for two machines. The script makes the room, starts
both agents, and stops at the approve step to ask you. Say `y` and the
notes are published; say anything else and the planner stops. Set
`AUTO_APPROVE=1` to skip the prompt.

No model key is needed: the CrewAI side runs a CrewAI Flow that flags
risky changes by keyword. Set `CREW_MODEL=anthropic/claude-sonnet-5` (plus
`ANTHROPIC_API_KEY`) and it runs a real one-agent Crew instead.

What you see (from a real run):

```
[planner] delegated to crew: task m_01M35YR3V7TPZTSN7PEHT1GGHW (seq 4)
[planner] sigcheck crew #5 claim: ok in 524 us
[planner] sigcheck crew #6 done: ok in 340 us
[planner] crew finished: 'Needs care: Drop the legacy `sessions_v1` table; Rotate the auth token signing secret'
[planner] asking a human to approve publishing notes sha256:72533907779901c1...
[planner] sigcheck owner #8 approve: ok in 262 us
[planner] human answer: approve from owner
[planner] gate: approved by owner (m_01M35YR86D08G5EWYN910TGN7Y), valid until 2026-09-23T01:50:44Z. Spent. (exit 0)
[planner] published /tmp/dvb.pcma/release-2.4.0.md
```

## Run it on two machines

Machine A (you and the planner):

```sh
diavlos new bridge
diavlos invite bridge planner            # for this machine
diavlos invite bridge crew               # send this one to machine B
diavlos --as planner join <planner invite>
python langgraph_planner.py --as planner --to crew
```

Machine B (the crew):

```sh
diavlos --as crew join <crew invite>
python crewai_worker.py --as crew
```

When the planner asks, approve from machine A with your own (human) key:

```sh
diavlos read bridge                                  # find the question id
diavlos send bridge --type approve --reply-to <id>   # or: diavlos deny bridge <id> --reason "..."
```

## Signature check: the numbers

`bench_sigcheck.py` has the CrewAI side send 1,000 signed messages. The
LangGraph side reads them from its own helper and verifies each Ed25519
signature in Python, over the exact bytes in [SPEC 2.3](../../docs/SPEC.md),
timing only the check. `run_local.sh` runs it at the end.
[benchmark-log.txt](benchmark-log.txt) is a real run:

```
messages checked      1000  (bad signatures: 0)
per message, p50         129.5 us
per message, p99         225.9 us
per message, max         499.3 us   (0.499 ms, budget 10 ms)
diavlos verify, whole bundle of 1009 messages (signatures + chain, incl. process start): 71.1 ms, exit 0
```

About 0.13 ms per message, 75 times under a 10 ms budget. The slowest
single check across runs we saw was 4.6 ms (a cold first call); still under.

The benchmark lifts the room's flood limit (60 messages a minute per
sender) on the local home only. Keep it on in real rooms.

## What each check buys you

- **The helper already checks.** Every signature, every chain link, before
  your code sees a message. The Python check is a second, independent one
  that trusts nothing but the member keys: `diavlos.sigcheck.KeyRing`.
- **Keys are pinned.** The planner compares each member key to the
  fingerprint `diavlos who` shows and stops on a mismatch.
- **A message only carries words.** The worker only reviews the change
  list it is given. The planner only publishes after a human approve.
- **One approve, one deed.** The approve names the sha256 of the notes.
  Change one byte and `check-approve` refuses. Use it twice and it refuses.

## Files

| File | What it is |
|---|---|
| `langgraph_planner.py` | The LangGraph graph: delegate, wait, ask a human, publish. |
| `crewai_worker.py` | The CrewAI side: offline Flow, or a real Crew with `CREW_MODEL`. |
| `bench_sigcheck.py` | 1,000 signed messages, each signature checked and timed. |
| `run_local.sh` | The whole thing on one machine. |
| `benchmark-log.txt` | A real benchmark run. |
