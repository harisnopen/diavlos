# Two E2B sandboxes, one Diavlos room, one human gate

Two isolated [E2B](https://e2b.dev) sandboxes send each other signed
messages through Diavlos. The agent in sandbox B will not run anything
until a human approves the exact command from the CLI, and a gate checks
that approve right before the command runs.

```
 your machine (the human, room owner)
   diavlos send --type approve  ────────────┐
                                             ▼
 sandbox A: planner ── signed task ──▶ sandbox B: runner
            ◀────── signed done ─────   asks, waits for a human approve,
                                        diavlos check-approve, then runs job.py
```

No open ports and no shared secret: each sandbox has its own key, and the
helpers find each other through relays.

## Try it without E2B first

```sh
pip install -r requirements.txt
python run.py --local                  # you approve at the prompt
python run.py --local --deny           # say no, and watch nothing run
```

`--local` runs the same agents with two local helpers standing in for the
two sandboxes. A real run (paths shortened):

```
[runner] planner wants the job run. Asking a human first.
[host] exact action: {"verb": "exec", "target": "sandbox-b", "params": {"cmd": "python .../agents/job.py"}}
sent m_01M35Z226M4P7447J07X04SGF2 (seq 7)
[runner] approve from owner
[runner] gate: approved by owner (m_01M35Z226M4P7447J07X04SGF2), valid until 2026-09-23T01:56:06Z. Spent. (exit 0)
[runner] job exit 0: 'nightly job ok on vm: sha256 2f3bf7d3e4a76a85 in 40 ms'
[planner] sigcheck runner #8 done: ok in 288 us
[host] audit bundle: OK: every signature verifies and the chain is whole.
```

## Run it on E2B

```sh
export E2B_API_KEY=...
python template.py          # once: builds the `diavlos-agents` template
python run.py               # two real sandboxes; you approve here
```

`template.py` builds a Python 3.12 sandbox with the `diavlos` CLI (from
the signed GitHub release) and the Python binding. `run.py` makes a room in
your own Diavlos home, starts two sandboxes from the template, gives each
agent its own one-time invite, and waits for you at the approve step.
Both sandboxes are killed at the end, and you keep a signed audit bundle.

The `--local` mode was run end to end for this example. The E2B mode uses
the same agents and the E2B Python SDK (`Sandbox.create`,
`commands.run`, `files.write`, `Template.build`), but has not yet been run
against a live E2B account: if something breaks there, please open an
issue.

## Why the human gate holds

- **The command is fixed in `runner.py`.** It never comes from a message:
  a task only says "please".
- **Only a human key can approve.** An agent that sends `approve` is
  refused by the room itself.
- **The approve is for exactly this command in exactly this sandbox.**
  Change a byte of the action and `check-approve` refuses.
- **One approve, one run.** The gate spends it. It also dies after ten
  minutes.

## Files

| File | What it is |
|---|---|
| `template.py` | Builds the `diavlos-agents` E2B template. |
| `run.py` | Makes the room, starts both sandboxes, asks you, prints the audit. |
| `agents/planner.py` | Sandbox A: sends the task, waits for a signed result. |
| `agents/runner.py` | Sandbox B: asks a human, runs the gate, then the job. |
| `agents/job.py` | The one job the runner may run. |
