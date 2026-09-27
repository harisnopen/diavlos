"""Film docs/FILES.md on two Orgo desktops, one step at a time.

`orgo.bash(box, cmd)` runs one shell command on that desktop through the
Orgo API (POST /api/computers/<id>/bash). That module holds account details
and is not included. Each command here calls do.sh on the desktop, which
types the command into that desktop's window and runs it there.

Desktop A is the room's home. Desktop B holds one agent key, agent-b.
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
    log(f"{role}$ {cmd[:90]}")
    bash(BOX[role], f"/root/dvf/film/do.sh {role} {shlex.quote(cmd)}")
    time.sleep(pause)

def note(role, text, pause=2.0):
    log(f"{role}# {text}")
    bash(BOX[role], f"/root/dvf/film/do.sh {role} '#' {shlex.quote(text)}")
    time.sleep(pause)

def last(role):
    return bash(BOX[role], f"cat /root/dvf/film/{role}.last")

def file_id(role):
    return re.search(r"sha256:([0-9a-f]{64})", last(role)).group(1)[:12]

note("a", "Desktop A. Install diavlos 2.1.0 with install.sh.", 0.5)
note("b", "Desktop B. Install diavlos 2.1.0 with install.sh.", 0.5)
run("a", INSTALL, 0.5)
run("b", INSTALL, 0.5)
run("a", "diavlos --version", 0.5)
run("b", "diavlos --version", 2.0)

note("b", "This desktop's node id, for the invite.")
run("b", "diavlos status | head -1", 1.0)
node = re.search(r"node ([0-9a-f]{64})", last("b")).group(1)

note("a", "1. Make a room. Invite agent-b, pinned to desktop B.")
run("a", "diavlos new ops", 1.5)
run("a", f"diavlos invite ops agent-b --for {node} | head -3", 1.0)
tok = re.search(r"dv1\.[A-Za-z0-9_=-]+", last("a")).group(0)
note("b", "Paste the join line from desktop A.")
run("b", f"diavlos --as agent-b join {tok}", 2.0)
run("a", "diavlos who ops | cut -c1-60", 4.0)

note("a", "2. Send a file to agent-b.")
run("a", "cat app.conf", 1.0)
run("a", 'diavlos send ops "config for the api, please check it" --file app.conf', 1.0)
fid = file_id("a")
run("b", "diavlos --as agent-b next ops --timeout 60", 2.0)
run("b", f"diavlos --as agent-b get ops {fid}", 2.0)
note("b", "get checked the SHA-256 fingerprint. Same by hand:", 0.5)
run("b", "sha256sum ~/.diavlos/files/ops/app.conf", 1.0)
note("b", "Same as the file id in the message. The check passed.", 4.0)

note("b", "3. agent-b changes the file and sends it back.")
run("b", "cp ~/.diavlos/files/ops/app.conf . && sed -i 's/workers = 2/workers = 8/' app.conf && cat app.conf", 1.5)
run("b", 'diavlos --as agent-b send ops "workers 2 -> 8, please use this one" --file app.conf', 1.0)
fid = file_id("b")
run("a", "diavlos next ops --timeout 60", 2.0)
run("a", f"diavlos get ops {fid}", 2.0)
run("a", "sha256sum ~/.diavlos/files/ops/app.conf", 1.0)
note("a", "The new version. What changed:", 0.5)
run("a", "diff app.conf ~/.diavlos/files/ops/app.conf", 4.0)

note("a", "4. Now a script file.")
run("a", "cat restart.sh", 1.0)
run("a", 'diavlos send ops "restart script, for later" --file restart.sh', 1.0)
fid = file_id("a")
run("b", "diavlos --as agent-b next ops --timeout 60", 2.0)
run("b", f"diavlos --as agent-b get ops {fid}", 3.0)
run("b", "ls -l ~/.diavlos/files/ops/", 2.0)
note("b", "Saved as restart.sh.unsafe, not runnable, with a warning. Nothing ran.", 4.0)

note("a", "Done. The guide is docs/FILES.md.", 0.5)
note("b", "Done. The guide is docs/FILES.md.", 5.0)
log("end")
