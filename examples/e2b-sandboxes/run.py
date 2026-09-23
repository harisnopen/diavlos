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
AGENT_NAMES = ("planner", "runner")


def log(*a):
    print(time.strftime("%H:%M:%S"), "[host]", *a, flush=True)


def cli(home, *args, env=None):
    cmd = [BIN] + (["--home", home] if home else []) + list(args)
    return subprocess.run(cmd, capture_output=True, text=True, check=True,
                          env=env).stdout


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
    """A real E2B sandbox from the `diavlos-agents` template.

    Everything long-lived (the helper, the agent) runs as a detached
    background job with its output in a log file, which a thread copies to
    this terminal. E2B commands themselves stay short."""

    HOME = "/home/user"

    def __init__(self, template, name):
        from e2b import Sandbox
        self.name = name
        self.sbx = Sandbox.create(template=template, timeout=1200, metadata={"diavlos": name})
        log(f"{name}: E2B sandbox {self.sbx.sandbox_id}")
        for f in ("planner.py", "runner.py", "job.py"):
            with open(os.path.join(AGENTS, f)) as fh:
                self.sbx.files.write(f"{self.HOME}/agents/{f}", fh.read())
        self.script = None
        self._stop = threading.Event()
        # Start the helper as its own long-lived process and wait for its socket.
        self.sh(f"setsid nohup diavlos helper > {self.HOME}/helper.log 2>&1 < /dev/null &")
        self.sh(f"for i in $(seq 100); do [ -S {self.HOME}/.diavlos/helper.sock ] && exit 0; sleep 0.1; done; "
                f"cat {self.HOME}/helper.log; exit 1")

    def sh(self, cmd, envs=None, timeout=60):
        return self.sbx.commands.run(cmd, envs=envs, timeout=timeout).stdout

    def join(self, agent, inv):
        self.sh(f'diavlos --as {agent} join "$INVITE"', envs={"INVITE": inv})

    def start(self, script, envs):
        self.script = script
        path = f"{self.HOME}/agents/{script}"
        self.sh(f"cd {self.HOME}/agents && setsid nohup sh -c 'python3 -u {path}; echo $? > {path}.rc' "
                f"> {path}.log 2>&1 < /dev/null &", envs=envs)
        threading.Thread(target=self._tail, args=(f"{path}.log",), daemon=True).start()

    def _tail(self, path):
        seen = 0
        while not self._stop.is_set():
            try:
                text = self.sbx.files.read(path)
            except Exception:
                text = ""
            for line in text.splitlines()[seen:]:
                print(f"  {self.name} | {line}", flush=True)
            seen = max(seen, len(text.splitlines()))
            time.sleep(1)

    def wait(self, timeout):
        rc_file = f"{self.HOME}/agents/{self.script}.rc"
        deadline = time.time() + timeout
        while time.time() < deadline:
            out = self.sh(f"cat {rc_file} 2>/dev/null || true").strip()
            if out:
                time.sleep(1.5)   # let the tail thread print the last lines
                return int(out)
            time.sleep(1)
        return None

    def dump(self):
        """What happened inside, for when something went wrong."""
        out = self.sh(f"tail -n 30 {self.HOME}/helper.log; echo ---; diavlos status 2>&1 | tail -n 20; "
                      f"echo ---; ls {self.HOME}/agents; tail -n 30 {self.HOME}/agents/*.log 2>/dev/null; true")
        for line in out.splitlines():
            print(f"  {self.name} (debug) | {line}", flush=True)

    def close(self):
        self._stop.set()
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
        # The owner is named after $USER. On a GitHub runner that is "runner",
        # which would clash with the agent of the same name.
        owner_env = dict(os.environ)
        if owner_env.get("USER") in AGENT_NAMES or owner_env.get("USERNAME") in AGENT_NAMES:
            owner_env["USER"] = owner_env["USERNAME"] = "human"
        cli(host, "new", room, "--about", "two E2B sandboxes, one human gate", env=owner_env)
        log(f"made room {room}; you are the owner and the approver")
        make = (lambda n: LocalBox(base, n)) if args.local else (lambda n: E2BBox(args.template, n))
        a, b = make("sandbox-a"), make("sandbox-b")
        boxes = [a, b]
        a.join("planner", invite(host, room, "planner"))
        b.join("runner", invite(host, room, "runner"))
        print(cli(host, "who", room), flush=True)

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
            if rc != 0 and hasattr(box, "dump"):
                try:
                    box.dump()
                except Exception as e:  # the sandbox may already be gone
                    log(f"{box.name}: no debug output ({e})")
            box.close()
        if args.local:
            subprocess.run([BIN, "--home", host, "stop"], capture_output=True)
    sys.exit(rc)


if __name__ == "__main__":
    main()
