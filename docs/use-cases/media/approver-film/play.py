"""Play docs/APPROVALS.md on the two Orgo desktops, one step at a time.

`orgo.bash(box, cmd)` runs one shell command on that desktop through the
Orgo API (POST /api/computers/<id>/bash). That module holds account details
and is not included. Each command here calls do.sh on the desktop, which
types the command into the right window and runs it there.
"""
import re, shlex, sys, time
sys.path.insert(0, sys.argv[1])
from orgo import bash

BOX = {"gate": "claude", "laptop": "claude", "agents": "openclaw"}
A = '{"verb":"deploy","target":"api-service","params":{"version":"1.2","env":"prod"}}'
T0 = time.time()

def log(msg):
    print(f"+{time.time() - T0:5.1f}s {msg}", flush=True)

def run(role, cmd, pause=3.0):
    log(f"{role}$ {cmd[:90]}")
    bash(BOX[role], f"/root/dvg/film/do.sh {role} {shlex.quote(cmd)}")
    time.sleep(pause)

def note(role, text, pause=2.0):
    log(f"{role}# {text}")
    bash(BOX[role], f"/root/dvg/film/do.sh {role} '#' {shlex.quote(text)}")
    time.sleep(pause)

def get(role, cmd):
    return bash(BOX[role], f". /root/dvg/film/{role}.env; {cmd}").strip()

def token(role):
    return get(role, f"grep -o 'dv1\\.[A-Za-z0-9_=-]*' /root/dvg/film/{role}.last | head -1")

def node(role):
    return re.search(r"node ([0-9a-f]{64})", get(role, "cat /root/dvg/film/%s.last" % role)).group(1)

note("gate", "Step 1. Make the room here. Its home and owner key never leave.")
run("gate", "diavlos new ops")

note("agents", "Step 2. Note this machine's node id. Give the coding agent its own key.")
run("agents", "diavlos status | head -1", 1.0)
an = node("agents")
run("agents", "diavlos mcp install --for claude-code | head -2")

note("gate", "Step 3. Invite each agent key, pinned to the agents machine.")
run("gate", f"diavlos invite ops claude-code --for {an}", 1.0)
cc = token("gate")
run("gate", f"diavlos invite ops deployer --for {an}", 1.0)
dep = token("gate")

note("agents", "Step 4. Paste the join line the gate printed, --as and all.")
run("agents", f"diavlos --as claude-code join {cc}", 1.5)
run("agents", f"diavlos --as deployer join {dep}")

note("laptop", "Step 5. Alice joins from her own laptop, as a person.")
run("laptop", "diavlos status | head -1", 1.0)
ln = node("laptop")
note("gate", "Invite Alice as a human approver, pinned to her laptop.")
run("gate", f"diavlos invite ops alice --human --for {ln}", 1.0)
al = token("gate")
run("laptop", f"diavlos join {al}")
run("gate", "diavlos who ops | cut -c1-40", 4.0)

note("agents", "The agent asks to deploy. It cannot approve; it waits for a person.")
ask = f"diavlos --as claude-code ask ops 'Deploy api-service v1.2 to prod?' --action '{A}' --timeout 300"
log("agents$ (in the background) " + ask[:70])
bash("openclaw", f"nohup /root/dvg/film/do.sh agents {shlex.quote(ask)} >/dev/null 2>&1 &")
time.sleep(6)

note("laptop", "Alice sees the exact action the approve will sign, and its id.")
run("laptop", "diavlos next ops --timeout 60", 4.0)
qid = get("laptop", "awk '$1==\"id\"{print $2}' /root/dvg/film/laptop.last")
note("laptop", "She read the action. She approves exactly that.")
run("laptop", f"diavlos send ops --type approve --reply-to {qid}", 5.0)

note("agents", "Step 6. The deploy script spends the approve at the gate, once.")
run("agents", f"diavlos --as deployer check-approve ops --op run-1 '{A}' && echo 'ok: deploying api-service v1.2'", 3.0)
note("agents", "The same run again gets the same answer. Nothing new is spent.")
run("agents", f"diavlos --as deployer check-approve ops --op run-1 '{A}'; echo \"exit $?\"", 3.0)
note("agents", "Any other run is told no.")
run("agents", f"diavlos --as deployer check-approve ops --op run-2 '{A}'; echo \"exit $?\"", 3.0)

note("gate", "Check it: doctor on every machine. No WARN anywhere.")
run("gate", "diavlos doctor | grep -E 'room |WARN'", 1.0)
run("laptop", "diavlos doctor | grep -E 'room |WARN'", 4.0)

note("gate", "Now the mistake the guide warns about: a person's key on the agents machine.")
run("gate", f"diavlos invite ops bob --human --for {an}", 1.0)
bob = token("gate")
note("agents", "Someone joins as a person, right here next to the agents.")
run("agents", f"diavlos join {bob}", 9.0)
note("agents", "Doctor on the right now warns. So does status:")
run("agents", "diavlos status | grep '!'", 5.0)

note("gate", "Fix: revoke bob on the gate.")
run("gate", "diavlos revoke ops bob", 12.0)
note("agents", "The warning cleared once this machine heard of the revoke.", 3.0)
note("gate", "Done. The setup is docs/APPROVALS.md.", 1.0)
note("agents", "Done. The setup is docs/APPROVALS.md.", 5.0)
log("end")
