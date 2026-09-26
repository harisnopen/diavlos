"""Two free Railway VMs, one Diavlos room, one human gate.

    python run.py                  # two real VMs from `ssh railway.new`
    python run.py --auto-approve   # no prompt (for CI)
    python run.py --deny           # say no, to see the gate hold
    python run.py --fake           # two local folders stand in for the VMs

Railway hands out a Linux VM to anyone who runs `ssh railway.new`, with no
account: it knows you by your SSH key. This script makes two fresh keys, so
it gets two VMs, and then:

1. On this machine you make a room. You are its owner and its human.
2. VM A gets the `planner` agent, VM B the `runner` agent (the same agents
   as examples/e2b-sandboxes). Each joins with its own one-time invite and
   its own Ed25519 key.
3. The planner sends the runner a signed task: run the nightly job.
4. The runner asks the room for approval with the exact command. Nothing
   runs yet.
5. You approve (or deny) here, with your human key.
6. The runner's gate (`diavlos check-approve`) checks that approve against
   the exact command, spends it, and only then runs the job.
7. The result goes back to the planner, signed, and you keep a signed audit
   bundle of the whole run, checked with `diavlos verify`.

Everything goes over plain SSH: files go up through stdin, long-running
jobs are started with `setsid` and read back with `cat`. The VMs open no
ports; the helpers find each other through relays.

Unclaimed Railway VMs are deleted by Railway after the claim window. This
script never prints the claim links, so nobody reading a CI log can claim
your VMs.
"""
import argparse
import json
import os
import platform
import re
import secrets
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
AGENTS = os.path.join(HERE, "..", "e2b-sandboxes", "agents")
BINDING = os.path.join(HERE, "..", "..", "bindings", "python")
BIN = os.environ.get("DIAVLOS_BIN") or shutil.which("diavlos") or "diavlos"
HOST = os.environ.get("RAILWAY_HOST", "railway.new")
AGENT_NAMES = ("planner", "runner")
IN_CI = os.environ.get("GITHUB_ACTIONS") == "true"


def log(*a):
    print(time.strftime("%H:%M:%S"), "[host]", *a, flush=True)


def cli(home, *args, env=None):
    cmd = [BIN] + (["--home", home] if home else []) + list(args)
    return subprocess.run(cmd, capture_output=True, text=True, check=True,
                          env=env, timeout=120).stdout


def invite(home, room, name):
    return next(w for w in cli(home, "invite", room, name).split() if w.startswith("dv1."))


def secret(value):
    """Keep a value out of a public CI log."""
    if IN_CI:
        print(f"::add-mask::{value}", flush=True)


def manifest_from(stderr):
    """Railway's first connect puts a JSON manifest on stderr. Its exact
    shape is not documented, so take the first JSON object we can read."""
    for line in stderr.splitlines():
        line = line.strip()
        if line.startswith("{"):
            try:
                return json.loads(line)
            except ValueError:
                pass
    start, end = stderr.find("{"), stderr.rfind("}")
    if 0 <= start < end:
        try:
            return json.loads(stderr[start:end + 1])
        except ValueError:
            pass
    return None


def walk(obj, path=""):
    if isinstance(obj, dict):
        for k, v in obj.items():
            yield from walk(v, f"{path}.{k}" if path else k)
    elif isinstance(obj, list):
        for i, v in enumerate(obj):
            yield from walk(v, f"{path}[{i}]")
    else:
        yield path, obj


# -- one VM, reached over SSH ---------------------------------------------------

class Box:
    """A Railway VM behind `ssh railway.new`, or with `fake=True` a local
    folder reached through `sh -c`, so the same code can be tried offline."""

    def __init__(self, base, name, fake=False):
        self.name, self.fake = name, fake
        self._stop = threading.Event()
        self.script = None
        if fake:
            self.root = os.path.join(base, name)
            os.makedirs(self.root)
        else:
            key = os.path.join(base, f"{name}.key")
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", f"diavlos-{name}",
                            "-f", key], check=True)
            self.ssh = ["ssh", "-i", key, "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes",
                        "-o", "StrictHostKeyChecking=accept-new",
                        "-o", f"UserKnownHostsFile={base}/known_hosts",
                        "-o", "ConnectTimeout=30", "-o", "ServerAliveInterval=15",
                        "-o", "ControlMaster=auto", "-o", f"ControlPath={base}/cm-{name}",
                        "-o", "ControlPersist=300", "-o", "LogLevel=ERROR", HOST]
        self.hello()

    # The one primitive: run a shell command on the box.
    def run(self, cmd, stdin=None, timeout=120, check=True):
        prefix = 'export PATH="$HOME/dv/bin:$HOME/.local/bin:$PATH"; '
        if self.fake:
            argv = ["sh", "-c", prefix + cmd]
            env = dict(os.environ, HOME=self.root)
            env.pop("DIAVLOS_HOME", None)
        else:
            argv = self.ssh + ["sh -c " + shlex.quote(prefix + cmd)]
            env = None
        p = subprocess.run(argv, input=stdin, capture_output=True, timeout=timeout, env=env)
        if check and p.returncode != 0:
            raise RuntimeError(f"{self.name}: `{cmd[:80]}` exit {p.returncode}: "
                               f"{p.stderr.decode(errors='replace').strip()[-800:]}")
        return p

    def sh(self, cmd, envs=None, timeout=120):
        exports = "".join(f"export {k}={shlex.quote(v)}; " for k, v in (envs or {}).items())
        return self.run(exports + cmd, timeout=timeout).stdout.decode()

    def hello(self):
        p = self.run("uname -sm; python3 --version", timeout=180, check=False)
        err = p.stderr.decode(errors="replace")
        if p.returncode != 0:
            hint = ""
            if re.search(r"limit|too many|quota|capacity", err, re.I):
                hint = " (Railway allows 3 free VMs per IP address per day, and caps them per region)"
            raise SystemExit(f"{self.name}: could not get a VM{hint}: {err.strip()[-800:]}")
        self.arch = p.stdout.decode().split()[1] if p.stdout else ""
        log(f"{self.name}: {' / '.join(p.stdout.decode().split(chr(10))[:2])}")
        if self.fake:
            return
        m = manifest_from(err)
        if not m:
            log(f"{self.name}: no manifest on stderr (Railway may have changed it)")
            return
        for path, value in walk(m):
            if isinstance(value, str) and re.search(r"claim", path + value, re.I):
                secret(value)   # never print a claim link
                log(f"{self.name}: {path} = (hidden)")
            elif isinstance(value, (str, int, float, bool)):
                log(f"{self.name}: {path} = {value}")

    def put_tree(self, local, remote):
        """Copy a folder up through stdin as a tar stream."""
        buf = tempfile.TemporaryFile()
        with tarfile.open(fileobj=buf, mode="w:gz") as t:
            t.add(local, arcname=".",
                  filter=lambda i: None if "__pycache__" in i.name else i)
        buf.seek(0)
        self.run(f"mkdir -p {remote} && tar xzf - -C {remote}", stdin=buf.read())

    def setup(self, upload_bin):
        self.put_tree(AGENTS, "$HOME/dv/agents")
        self.put_tree(BINDING, "$HOME/dv/py")
        if upload_bin:
            with open(BIN, "rb") as f:
                self.run("mkdir -p $HOME/dv/bin && cat > $HOME/dv/bin/diavlos && chmod +x $HOME/dv/bin/diavlos",
                         stdin=f.read(), timeout=300)
        else:
            version = "v" + cli(None, "--version").split()[-1]
            self.sh("curl -fsSL https://raw.githubusercontent.com/harisnopen/diavlos/main/install.sh | "
                    f"DIAVLOS_VERSION={shlex.quote(version)} DIAVLOS_INSTALL_DIR=$HOME/dv/bin sh", timeout=300)
        # The agents check signatures themselves, which needs `cryptography`.
        self.sh("python3 -m venv $HOME/dv/venv && $HOME/dv/venv/bin/pip install -q cryptography",
                timeout=300)
        self.python = "$HOME/dv/venv/bin/python"
        if self.fake:
            # Offline: no relays, loopback only.
            self.sh("mkdir -p $HOME/.diavlos && printf '[helper]\\npublic_relays = false\\nretry_secs = 1\\n' "
                    "> $HOME/.diavlos/config.toml")
        log(f"{self.name}: " + self.sh("diavlos --version").strip())
        self.sh("setsid -f diavlos helper > $HOME/dv/helper.log 2>&1 < /dev/null")
        self.sh("for i in $(seq 100); do [ -S $HOME/.diavlos/helper.sock ] && exit 0; sleep 0.1; done; "
                "cat $HOME/dv/helper.log; exit 1")

    def join(self, agent, inv):
        self.sh(f'diavlos --as {agent} join "$INVITE"', envs={"INVITE": inv})

    def start(self, script, envs):
        self.script = script
        path = f"$HOME/dv/agents/{script}"
        envs = dict(envs, PYTHONPATH="$HOME/dv/py")
        exports = "".join(f"export {k}={v if v.startswith('$HOME') else shlex.quote(v)}; "
                          for k, v in envs.items())
        self.run(exports + f"cd $HOME/dv/agents && setsid -f sh -c '{self.python} -u {path}; "
                 f"echo $? > {path}.rc' > {path}.log 2>&1 < /dev/null")
        threading.Thread(target=self._tail, args=(f"{path}.log",), daemon=True).start()

    def _tail(self, path):
        seen = 0
        while not self._stop.is_set():
            try:
                lines = self.sh(f"cat {path} 2>/dev/null || true").splitlines()
            except Exception:
                lines = []
            for line in lines[seen:]:
                print(f"  {self.name} | {line}", flush=True)
            seen = max(seen, len(lines))
            time.sleep(1.5)

    def wait(self, timeout):
        rc_file = f"$HOME/dv/agents/{self.script}.rc"
        deadline = time.time() + timeout
        while time.time() < deadline:
            out = self.sh(f"cat {rc_file} 2>/dev/null || true").strip()
            if out:
                time.sleep(2)   # let the tail thread print the last lines
                return int(out)
            time.sleep(1.5)
        return None

    def exited(self):
        """The agent's exit code if it has stopped, else None."""
        if not self.script:
            return None
        out = self.sh(f"cat $HOME/dv/agents/{self.script}.rc 2>/dev/null || true").strip()
        return int(out) if out else None

    def dump(self):
        out = self.sh("tail -n 30 $HOME/dv/helper.log; echo ---; diavlos status 2>&1 | tail -n 20; "
                      "echo ---; tail -n 30 $HOME/dv/agents/*.log 2>/dev/null; true")
        for line in out.splitlines():
            print(f"  {self.name} (debug) | {line}", flush=True)

    def close(self):
        self._stop.set()
        try:
            self.run('diavlos stop; pkill -f "$HOME/dv/agents/" || true', check=False, timeout=30)
        except Exception:
            pass
        if not self.fake:
            subprocess.run(self.ssh[:-1] + ["-O", "exit", HOST], capture_output=True)


# -- the run ------------------------------------------------------------------

def find_question(home, room, boxes, timeout=600):
    deadline = time.time() + timeout
    while time.time() < deadline:
        lines = cli(home, "read", room, "--since", "1", "--json").splitlines()
        qs = [m for m in map(json.loads, lines) if m["type"] == "question" and m.get("action")]
        if qs:
            return qs[-1]
        for box in boxes:
            code = box.exited()
            if code is not None:
                time.sleep(2)   # let the tail thread print why
                raise RuntimeError(f"{box.name}: agent stopped (exit {code}) before asking")
        time.sleep(2)
    raise RuntimeError("no approval request arrived")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--fake", action="store_true", help="two local folders instead of Railway VMs")
    ap.add_argument("--auto-approve", action="store_true")
    ap.add_argument("--deny", action="store_true", help="say no, to see the gate hold")
    ap.add_argument("--install", action="store_true",
                    help="install diavlos on the VMs from the signed release instead of uploading this one")
    args = ap.parse_args()

    base = tempfile.mkdtemp(prefix="dvr-", dir="/tmp" if os.path.isdir("/tmp") else None)
    room = f"vm-{secrets.token_hex(3)}"
    if args.fake:
        host = os.path.join(base, "host")
        os.makedirs(host)
        with open(os.path.join(host, "config.toml"), "w") as f:
            f.write("[helper]\npublic_relays = false\nretry_secs = 1\n")
    else:
        if not shutil.which("ssh"):
            raise SystemExit("needs ssh (or try --fake)")
        host = os.environ.get("DIAVLOS_HOME")  # your own home: you are the human

    boxes = []
    rc = 1
    try:
        # The owner is named after $USER. On a GitHub runner that is "runner",
        # which would clash with the agent of the same name.
        owner_env = dict(os.environ)
        if owner_env.get("USER") in AGENT_NAMES or owner_env.get("USERNAME") in AGENT_NAMES:
            owner_env["USER"] = owner_env["USERNAME"] = "human"
        cli(host, "new", room, "--about", "two Railway VMs, one human gate", env=owner_env)
        log(f"made room {room}; you are the owner and the approver")

        a, b = Box(base, "vm-a", args.fake), Box(base, "vm-b", args.fake)
        boxes = [a, b]
        for box in boxes:
            # Same diavlos as this machine when it can run there, so every
            # helper in the room speaks the same version.
            same = (sys.platform == "linux" and platform.machine() == box.arch and not args.install)
            box.setup(upload_bin=same)
        a.join("planner", invite(host, room, "planner"))
        b.join("runner", invite(host, room, "runner"))
        print(cli(host, "who", room), flush=True)

        b.start("runner.py", {"DIAVLOS_ROOM": room, "DIAVLOS_ME": "runner", "SANDBOX_NAME": b.name})
        time.sleep(1)
        a.start("planner.py", {"DIAVLOS_ROOM": room, "DIAVLOS_ME": "planner", "DIAVLOS_PEER": "runner"})

        q = find_question(host, room, boxes)
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
        if v.returncode != 0:
            rc = 1
        print()
        print(cli(host, "read", room, "--since", "1"))
        log(f"bundle kept at {bundle}")
    finally:
        for box in boxes:
            if rc != 0:
                try:
                    box.dump()
                except Exception as e:  # the VM may already be gone
                    log(f"{box.name}: no debug output ({e})")
            box.close()
        if args.fake:
            subprocess.run([BIN, "--home", host, "stop"], capture_output=True)
    sys.exit(rc)


if __name__ == "__main__":
    main()
