"""What every play-*.py shares.

`orgo.bash(box, cmd)` runs one shell command on that desktop through the
Orgo API (POST /api/computers/<id>/bash). That module holds account details
and is not included; pass the folder it is in as the first argument. Each
on-camera step calls do.sh on the desktop, which types the command into
that desktop's window and runs it there.

Desktop A is the room's home. Desktop B holds agent keys.
"""
import re, shlex, sys, time
sys.path.insert(0, sys.argv[1])
from orgo import bash

BOX = {"a": "claude", "b": "openclaw"}
INSTALL = "curl -fsSL https://raw.githubusercontent.com/harisnopen/diavlos/main/install.sh | sh"
T0 = time.time()

def log(msg):
    print(f"+{time.time() - T0:5.1f}s {msg}", flush=True)

def run(role, cmd, pause=3.0):
    """On camera: type CMD in ROLE's window, run it, show the output."""
    log(f"{role}$ {cmd[:90]}")
    bash(BOX[role], f"/root/dvf/film/do.sh {role} {shlex.quote(cmd)}")
    time.sleep(pause)

def note(role, text, pause=2.0):
    log(f"{role}# {text}")
    bash(BOX[role], f"/root/dvf/film/do.sh {role} '#' {shlex.quote(text)}")
    time.sleep(pause)

def last(role):
    """The output of ROLE's last on-camera command."""
    return bash(BOX[role], f"cat /root/dvf/film/{role}.last")

def quiet(role, cmd):
    """Off camera, as ROLE."""
    return bash(BOX[role], f". /root/dvf/film/{role}.env; cd $HOME/work; {cmd}")

def fresh():
    """Off camera: clean homes, diavlos from install.sh on both desktops."""
    for role in ("a", "b"):
        bash(BOX[role], f"/root/dvf/film/setup.sh {role}")
        quiet(role, INSTALL + " >/dev/null")

def room(b_agents=(), a_agents=()):
    """Off camera: A makes room ops and invites agents on B (and on A)."""
    quiet("a", "diavlos new ops")
    nb = re.search(r"node ([0-9a-f]{64})", quiet("b", "diavlos status")).group(1)
    for role, names, node in (("b", b_agents, nb), ("a", a_agents, None)):
        for name in names:
            name, _, r = name.partition(":")  # "name:role" for a role other than task-giver
            pin = f" --for {node}" if node else ""
            pin += f" --role {r}" if r else ""
            tok = re.search(r"dv1\.[A-Za-z0-9_=-]+", quiet("a", f"diavlos invite ops {name}{pin}")).group(0)
            log(quiet(role, f"diavlos --as {name} join {tok}").strip())

def msg_id(role):
    return re.search(r"m_[0-9A-Z]{26}", last(role)).group(0)

BOX.update({"a2": "claude", "b2": "openclaw"})  # second windows

def together(*steps):
    """Run on-camera steps at the same moment, one thread each."""
    import threading
    ts = [threading.Thread(target=run, args=(role, cmd, 0)) for role, cmd in steps]
    [t.start() for t in ts]; [t.join() for t in ts]

on_ready = None  # set by take.py: stage the windows and start recording

def ready():
    """The room is set up off camera. Filming starts here."""
    if on_ready:
        on_ready()
