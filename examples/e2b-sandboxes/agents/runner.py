"""runner: lives in sandbox B. Takes a task, but runs nothing until a
human approves the exact command, and the gate agrees.

    DIAVLOS_ROOM=sbx DIAVLOS_ME=runner SANDBOX_NAME=sandbox-b python3 runner.py

The command is fixed here (JOB_CMD). It never comes from message text: a
task only says "please", the human's signed approve is the permission,
and `diavlos check-approve` checks it right before the command runs.
"""
import json
import os
import shlex
import subprocess
import sys
import time

try:
    from diavlos import DiavlosError, Room
    from diavlos.sigcheck import KeyRing
except ImportError:  # running from a checkout
    sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..", "bindings", "python"))
    from diavlos import DiavlosError, Room
    from diavlos.sigcheck import KeyRing

ROOM = os.environ["DIAVLOS_ROOM"]
ME = os.environ.get("DIAVLOS_ME", "runner")
SANDBOX = os.environ.get("SANDBOX_NAME", "sandbox-b")
HERE = os.path.dirname(os.path.abspath(__file__))
JOB_CMD = os.environ.get("JOB_CMD", f"{shlex.quote(sys.executable)} {shlex.quote(os.path.join(HERE, 'job.py'))}")
BIN = os.environ.get("DIAVLOS_BIN", "diavlos")


def log(*a):
    print(time.strftime("%H:%M:%S"), f"[{ME}]", *a, flush=True)


room = Room.open(ROOM, name=ME)
keys = KeyRing.load(ROOM, name=ME)
log(f"ready in {ROOM} as {ME} on {SANDBOX}")

for msg in room.next():
    ok, us = keys.check(msg)
    log(f"sigcheck {msg['from']} #{msg['seq']} {msg['type']}: {'ok' if ok else 'BAD'} in {us:.0f} us")
    if not ok or msg["type"] != "task" or msg.get("to") != ME:
        continue
    room.claim(msg["id"])
    action = {"verb": "exec", "target": SANDBOX, "params": {"cmd": JOB_CMD}}
    log(f"{msg['from']} wants the job run. Asking a human first.")
    try:
        answer = room.ask(f"{msg['from']} asks me to run `{JOB_CMD}` in {SANDBOX}. Approve?",
                          timeout=600, action=action, trace=msg.get("trace"))
    except DiavlosError as e:
        why = "a human said no" if e.code == 6 else f"no approval ({e})"
        log(why)
        room.send(f"Not run: {why}.", type="reply", reply_to=msg["id"], trace=msg.get("trace"))
        break
    log(f"{answer['type']} from {answer['from']}")
    if answer["type"] != "approve":
        room.send("Not run: a human said no.", type="reply", reply_to=msg["id"], trace=msg.get("trace"))
        break

    gate = subprocess.run([BIN, "--as", ME, "check-approve", ROOM, json.dumps(action)],
                          capture_output=True, text=True)
    log("gate:", (gate.stdout or gate.stderr).strip(), f"(exit {gate.returncode})")
    if gate.returncode != 0:
        room.send("Not run: the gate found no valid human approve.", type="reply",
                  reply_to=msg["id"], trace=msg.get("trace"))
        break

    run = subprocess.run(shlex.split(JOB_CMD), capture_output=True, text=True, timeout=300)
    out = (run.stdout + run.stderr).strip()[-2000:]
    log(f"job exit {run.returncode}: {out[:120]!r}")
    room.send(out or f"exit {run.returncode}", type="done", reply_to=msg["id"],
              trace=msg.get("trace"), data={"exit": run.returncode, "sandbox": SANDBOX})
    break
