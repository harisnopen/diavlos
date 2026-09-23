"""Two E2B sandboxes, one Diavlos room, one human gate.

    E2B_API_KEY=... python run.py            # real E2B sandboxes
    python run.py --local                    # same agents, two local helpers
    python run.py --local --auto-approve     # no prompt (for CI)

What happens:

1. On this machine you make a room. You are its owner and its human.
2. Sandbox A gets the `planner` agent, sandbox B the `runner` agent. Each
   joins the room with its own one-time invite and its own Ed25519 key.
3. The planner sends the runner a signed task: run the nightly job.
4. The runner asks the room for approval, with the exact command as a
   structured action. Nothing runs yet.
5. You approve (or deny) here, in the CLI, with your human key.
6. The runner's gate (`diavlos check-approve`) checks that approve against
   the exact command, spends it, and only then runs the job.
7. The result goes back to the planner, signed. You get a signed audit
   bundle of the whole run, checked with `diavlos verify`.

The sandboxes never talk to each other directly and need no open ports:
the helpers find each other through relays.
"""
import argparse
import json
import os
import secrets
import shutil
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
AGENTS = os.path.join(HERE, "agents")
BIN = os.environ.get("DIAVLOS_BIN") or shutil.which("diavlos") or "diavlos"


def log(*a):
    print(time.strftime("%H:%M:%S"), "[host]", *a, flush=True)


def cli(home, *args):
    cmd = [BIN] + (["--home", home] if home else []) + list(args)
    return subprocess.run(cmd, capture_output=True, text=True, check=True).stdout


def invite(home, room, name):
    return next(w for w in cli(home, "invite", room, name).split() if w.startswith("dv1."))


# -- where the agents run -------------------------------------------------------

class LocalBox:
    """A stand-in sandbox: its own Diavlos home and a Python process."""

    def __init__(self, base, name):
        self.name = name
        self.home = os.path.join(base, name)
        os.makedirs(self.home)
        with open(os.path.join(self.home, "config.toml"), "w") as f:
            f.write("[helper]\npublic_relays = false\nretry_secs = 1\n")
        self.proc = None

    def join(self, agent, inv):
        cli(self.home, "--as", agent, "join", inv)

    def start(self, script, envs):
        env = dict(os.environ, DIAVLOS_HOME=self.home, **envs)
        self.proc = subprocess.Popen([sys.executable, "-u", os.path.join(AGENTS, script)], env=env,
                                     stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        threading.Thread(target=lambda: [print(f"  {self.name} | {line}", end="", flush=True)
                                         for line in self.proc.stdout], daemon=True).start()

    def wait(self, timeout):
        try:
            return self.proc.wait(timeout)
        except subprocess.TimeoutExpired:
            return None

    def close(self):
        if self.proc and self.proc.poll() is None:
            self.proc.kill()
        subprocess.run([BIN, "--home", self.home, "stop"], capture_output=True)


class E2BBox:
    """A real E2B sandbox from the `diavlos-agents` template."""

    def __init__(self, template, name):
        from e2b import Sandbox
        self.name = name
        self.sbx = Sandbox.create(template=template, timeout=1200, metadata={"diavlos": name})
        log(f"{name}: E2B sandbox {self.sbx.sandbox_id}")
        for f in ("planner.py", "runner.py", "job.py"):
            with open(os.path.join(AGENTS, f)) as fh:
                self.sbx.files.write(f"/home/user/agents/{f}", fh.read())
        # Keep the helper as a process of its own, so it outlives each command.
        self.sbx.commands.run("diavlos helper", background=True, timeout=0)
        self.handle = None

    def join(self, agent, inv):
        self.sbx.commands.run(f'diavlos --as {agent} join "$INVITE"', envs={"INVITE": inv}, timeout=60)

    def start(self, script, envs):
        out = lambda line: print(f"  {self.name} | {line}", end="" if line.endswith("\n") else "\n", flush=True)  # noqa: E731
        self.handle = self.sbx.commands.run(f"python3 -u /home/user/agents/{script}", background=True,
                                            envs=envs, cwd="/home/user/agents", timeout=0,
                                            on_stdout=out, on_stderr=out)

    def wait(self, timeout):
        from e2b import CommandExitException
        result = [None]

        def w():
            try:
                result[0] = self.handle.wait().exit_code
            except CommandExitException as e:
                result[0] = e.exit_code
        t = threading.Thread(target=w, daemon=True)
        t.start()
        t.join(timeout)
        return result[0]

    def close(self):
        self.sbx.kill()


# -- the run ------------------------------------------------------------------

def find_question(home, room, timeout=600):
    deadline = time.time() + timeout
    while time.time() < deadline:
        lines = cli(home, "read", room, "--since", "1", "--json").splitlines()
        qs = [m for m in map(json.loads, lines) if m["type"] == "question" and m.get("action")]
        if qs:
            return qs[-1]
        time.sleep(1)
    raise SystemExit("no approval request arrived")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--local", action="store_true", help="two local helpers instead of E2B")
    ap.add_argument("--template", default=os.environ.get("E2B_TEMPLATE", "diavlos-agents"))
    ap.add_argument("--auto-approve", action="store_true")
    ap.add_argument("--deny", action="store_true", help="say no, to see the gate hold")
    args = ap.parse_args()

    base = tempfile.mkdtemp(prefix="dve-", dir="/tmp" if os.path.isdir("/tmp") else None)
    room = f"sbx-{secrets.token_hex(3)}"
    if args.local:
        host = os.path.join(base, "host")
        os.makedirs(host)
        with open(os.path.join(host, "config.toml"), "w") as f:
            f.write("[helper]\npublic_relays = false\nretry_secs = 1\n")
    else:
        if not os.environ.get("E2B_API_KEY"):
            raise SystemExit("set E2B_API_KEY, or use --local")
        host = os.environ.get("DIAVLOS_HOME")  # your own home: you are the human

    boxes = []
    rc = 1
    try:
        cli(host, "new", room, "--about", "two E2B sandboxes, one human gate")
        log(f"made room {room}; you are the owner and the approver")
        make = (lambda n: LocalBox(base, n)) if args.local else (lambda n: E2BBox(args.template, n))
        a, b = make("sandbox-a"), make("sandbox-b")
        boxes = [a, b]
        a.join("planner", invite(host, room, "planner"))
        b.join("runner", invite(host, room, "runner"))
        print(cli(host, "who", room))

        b.start("runner.py", {"DIAVLOS_ROOM": room, "DIAVLOS_ME": "runner", "SANDBOX_NAME": b.name})
        time.sleep(1)
        a.start("planner.py", {"DIAVLOS_ROOM": room, "DIAVLOS_ME": "planner", "DIAVLOS_PEER": "runner"})

        q = find_question(host, room)
        print()
        log(f"approval request #{q['seq']} from {q['from']}: {q['text']}")
        log(f"exact action: {json.dumps(q['action'])}")
        if args.deny:
            ans = "n"
        elif args.auto_approve:
            ans = "y"
        else:
            ans = input("You are the human. Approve exactly this? [y/N] ").strip().lower()
        if ans == "y":
            print(cli(host, "send", room, "--type", "approve", "--reply-to", q["id"]).strip())
        else:
            print(cli(host, "deny", room, q["id"], "--reason", "not approved at the CLI").strip())

        rc_a, rc_b = a.wait(300), b.wait(60)
        log(f"planner exit {rc_a}, runner exit {rc_b}")
        rc = 0 if (rc_a == 0) == (ans == "y") else 1

        bundle = os.path.join(base, f"{room}.bundle.jsonl")
        with open(bundle, "w") as f:
            f.write(cli(host, "export", room))
        v = subprocess.run([BIN, "verify", bundle], capture_output=True, text=True)
        log("audit bundle:", v.stdout.strip().splitlines()[-2] if v.returncode == 0 else v.stderr.strip())
        print()
        print(cli(host, "read", room, "--since", "1"))
        log(f"bundle kept at {bundle}")
    finally:
        for box in boxes:
            box.close()
        if args.local:
            subprocess.run([BIN, "--home", host, "stop"], capture_output=True)
    sys.exit(rc)


if __name__ == "__main__":
    main()
