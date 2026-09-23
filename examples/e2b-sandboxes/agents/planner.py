"""planner: lives in sandbox A. Hands one job to the runner in sandbox B
and waits for the result.

    DIAVLOS_ROOM=sbx DIAVLOS_ME=planner DIAVLOS_PEER=runner python3 planner.py
"""
import os
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
ME = os.environ.get("DIAVLOS_ME", "planner")
PEER = os.environ.get("DIAVLOS_PEER", "runner")


def log(*a):
    print(time.strftime("%H:%M:%S"), f"[{ME}]", *a, flush=True)


room = Room.open(ROOM, name=ME)
keys = KeyRing.load(ROOM, name=ME)
task = room.send("Run the nightly job in your sandbox and send me its output.",
                 type="task", to=PEER, trace="nightly")
log(f"sent task {task['id']} to {PEER}")

deadline = time.time() + 900
while time.time() < deadline:
    try:
        m = room.next_one(timeout=30)
    except DiavlosError as e:
        if e.code == 4:
            log(f"waiting for {PEER} ...")
            continue
        raise
    ok, us = keys.check(m)
    log(f"sigcheck {m['from']} #{m['seq']} {m['type']}: {'ok' if ok else 'BAD'} in {us:.0f} us")
    if not ok or m.get("reply_to") != task["id"] or m["from"] != PEER:
        continue
    if m["type"] == "claim":
        log(f"{PEER} claimed it")
    elif m["type"] == "done":
        log(f"{PEER} finished: {m['text']}")
        sys.exit(0)
    elif m["type"] == "reply":
        log(f"{PEER} did not run it: {m['text']}")
        sys.exit(6)
log("gave up waiting")
sys.exit(4)
