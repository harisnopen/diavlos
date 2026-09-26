# Two free Railway VMs, one Diavlos room, one human gate

[Railway](https://railway.com/free-vm) gives anyone a Linux VM with
`ssh railway.new`: no account, no card. It knows you by your SSH key. This
example gets two of them, puts an agent on each, and has them talk over
Diavlos. The agent on VM B will not run anything until a human approves
the exact command, and a gate checks that approve right before it runs.

```
 your machine (the human, room owner)
   approve / deny  ─────────────────────────┐
                                             ▼
 VM A: planner ── signed task ──▶ VM B: runner
        ◀────── signed done ─────   asks, waits for a human approve,
                                    diavlos check-approve, then runs job.py
```

The VMs open no ports and share no secret. Each has its own key, and the
helpers find each other through relays over HTTPS.

## Try it

You need `ssh`, Python 3, and `diavlos` on your PATH. No Railway account.

```sh
git clone https://github.com/harisnopen/diavlos && cd diavlos/examples/railway-vms
python run.py              # two real VMs; you approve at the prompt
python run.py --deny       # say no, and watch nothing run
python run.py --fake       # no Railway: two local folders stand in for the VMs
```

What `run.py` does:

1. Makes two new SSH keys, one per VM, so `ssh railway.new` hands out two
   VMs.
2. Makes a room on your machine. You are its owner and its human.
3. Puts `diavlos` on each VM: a copy of yours when the VM's CPU matches,
   or the signed release otherwise (`--install` forces that). It also
   copies the agents and the Python binding, all over plain SSH.
4. Gives each agent its own one-time invite. The planner on VM A sends a
   signed task, and the runner on VM B asks you before running anything.
5. You answer. The runner's gate checks your approve against the exact
   command and spends it, and only then runs the job.
6. You keep a signed audit bundle of the whole run, checked with
   `diavlos verify`.

The agents are the same ones as in [e2b-sandboxes](../e2b-sandboxes/), so
the same run works on either.

## Run it in GitHub Actions

`.github/workflows/railway-demo.yml` runs it on a GitHub runner. It needs
no secret. Start it from the Actions tab ("railway demo" → Run workflow)
and pick `approve` or `deny`. In a fork it works the same way.

## Good to know

- **Limits.** Railway allows 3 free VMs per IP address per day, and one
  run uses 2. If a run is refused, `run.py` says it hit the limit.
- **Claim links stay hidden.** Railway's first connect prints a welcome
  manifest with a preview URL and a claim link. `run.py` prints the
  manifest fields but never the claim link, and in GitHub Actions it masks
  it too, so nobody reading a public log can take your VMs.
- **Cleanup.** At the end `run.py` stops the helpers and agents. The VMs
  themselves are deleted by Railway once the claim window ends, unless
  you claim them.
- **What is not documented.** Railway does not yet document the manifest's
  fields or how it maps keys to VMs. `run.py` reads the manifest loosely
  and uses a fresh key per VM, so a change on Railway's side shows up as
  a plain error, not a wrong result.

## Checked so far

`--fake` passes on Linux both ways (approve runs the job, deny does not),
and `--install` gets 2.0.0 from the signed release. A run on real Railway
VMs goes through the workflow above; its output will be linked here once
it has run.
