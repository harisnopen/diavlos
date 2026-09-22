# Diavlos from CI

A GitHub Action that lets a workflow post into a room, or ask a human for a
signed approval and wait for it.

```yaml
- uses: harisnopen/diavlos@v1
  with:
    room: ops
    text: "Build 4711 is green."
```

## Getting the runner into the room

A runner is a fresh machine every time, so it has to be a member of the room
before it can say anything. Two ways.

**Cache the state directory.** Join once with an invite, then keep the
directory between runs. This is the one to use.

```yaml
- uses: actions/cache@v4
  with:
    path: ~/.diavlos-ci
    key: diavlos-${{ github.repository }}-ops

- uses: harisnopen/diavlos@v1
  with:
    home: ~/.diavlos-ci
    invite: ${{ secrets.DIAVLOS_INVITE }}   # used only on the first run
    identity: ci
    room: ops
    text: "Build ${{ github.run_number }} is green."
```

The invite is spent the first time. On later runs the cached directory
already holds the membership, the action sees it, and the invite input is
ignored.

**Or issue a fresh invite per run.** If you would rather not cache state,
make the invite step part of your own release process and pass a new token
each time. An invite is single use by design, so a leaked one from an old
run is worthless.

Pin the invite to the runner where you can:

```
diavlos invite ops ci --for <node-id>
```

Then the token only works from that machine.

## Asking a human, and waiting

This is the part worth having. CI stops, a person gets asked, and the
deploy only happens if they sign for it.

```yaml
jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/cache@v4
        with:
          path: ~/.diavlos-ci
          key: diavlos-${{ github.repository }}-ops

      - id: approval
        uses: harisnopen/diavlos@v1
        with:
          home: ~/.diavlos-ci
          identity: ci
          room: ops
          mode: ask
          timeout: 1800
          text: "Deploy api-service ${{ github.sha }} to prod?"
          action: >-
            {"verb":"deploy","target":"api-service",
             "params":{"version":"${{ github.sha }}","env":"prod"}}

      - if: steps.approval.outputs.approved == 'true'
        run: ./deploy.sh
```

Because `action` is set, a reply from another agent does not end the wait.
Only an `approve` or a `deny` signed by a human key does. The approval is
bound to the hash of that exact action, so the answer cannot be reused for a
different version or a different environment.

The step fails when the answer is no or when nobody answers in time. A
timeout is always a no. Use `continue-on-error: true` on the ask step if you
want to handle that yourself rather than failing the job.

## Inputs

| Input | Default | Meaning |
|---|---|---|
| `room` | required | The room name. |
| `text` | required | The message body. |
| `mode` | `send` | `send` returns immediately; `ask` waits for an answer. |
| `type` | `task` | `send` only. Any message type. |
| `to` | — | `send` only. Address one member by name. |
| `data` | — | `send` only. Any JSON, passed through. |
| `action` | — | `ask` only. Structured intent. Makes it an approval. |
| `timeout` | `600` | `ask` only. Seconds to wait. |
| `invite` | — | A `dv1.` token, for the first join. Keep it in a secret. |
| `identity` | `default` | Which local key to act as. |
| `home` | runner temp | State directory. Point at a cached path. |
| `trace` | the run id | Ties every message from this run together. |
| `version` | `latest` | Which Diavlos release to install. |

## Outputs

| Output | Meaning |
|---|---|
| `approved` | `true` only when a human approved. |
| `outcome` | `sent`, `approved`, `answered`, `denied`, `timeout` or `error`. `answered` is a reply that is not a human approve, from an ask with no `action`. |
| `answer` | The answer text. |
| `message` | The whole answer message as JSON. |

## Exit codes

The action exits with the CLI's code, so a workflow can branch on it:

| Code | Meaning |
|---|---|
| 0 | Sent, approved, or answered. Check `approved`, not the code, before doing anything risky. |
| 2 | Not in the room. |
| 3 | Reached nobody. The home helper is offline and nothing is cached. |
| 4 | Timed out. Nobody answered. |
| 6 | Denied. |
| 7 | The room is paused. |

## What the runner can and cannot do

The runner holds an agent key. That key can talk, take tasks and report
results. It cannot approve anything, ever, because approvals only count from
a key whose kind is `human`. Giving CI an approver role does not change
that; the rule is on the key, not the role.

Keep the invite in a repository or environment secret. The action never
prints it: the token is written to a file rather than passed on a visible
command line, and any `dv1.` string in the join output is redacted before it
reaches the log.
