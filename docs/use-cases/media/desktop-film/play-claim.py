"""Film: two agents on two desktops claim one task at the same moment.
One wins, the other is told no. See film.py.
Desktop A's left window is desk-a (a), its right one is agent w1 (a2)."""
from film import *

fresh(); room(b_agents=["w2"], a_agents=["w1"])
ready()

def winner(tid):
    return "a2" if "claimed" in last("a2") else "b"

NAME = {"a2": "w1", "b": "w2"}
OTHER = {"a2": "b", "b": "a2"}

note("a", "desk-a runs the room. w1 is an agent on this desktop, w2 on desktop B.", 0.5)
run("a", "diavlos who ops | cut -c1-44", 1.0)
note("a", "Post one task. Both agents want it.", 0.5)
run("a", "diavlos send ops --type task 'rotate the logs on web-1'", 0.5)
tid = msg_id("a")
together(("a2", "diavlos --as w1 next ops --timeout 60"), ("b", "diavlos --as w2 next ops --timeout 60"))
time.sleep(2)
note("a2", "Both claim it, at the same moment.", 0)
note("b", "Both claim it, at the same moment.", 1.0)
together(("a2", f"diavlos --as w1 claim ops {tid}"), ("b", f"diavlos --as w2 claim ops {tid}"))
time.sleep(3)
w = winner(tid); l = OTHER[w]
note(w, f"{NAME[w]} won. It does the work and says done.", 0.5)
run(w, f"diavlos --as {NAME[w]} send ops --type done --reply-to {tid} 'logs rotated'", 1.0)
note(l, f"{NAME[l]} was told no. It tries again:", 0.5)
run(l, f"diavlos --as {NAME[l]} claim ops {tid}; echo \"exit $?\"", 3.0)
note(l, "Still no. No double work.", 3.0)

for role in ("a2", "b"):  # off camera: read past the claims and dones
    quiet(role, f"while diavlos --as {NAME[role]} next ops --timeout 3 >/dev/null 2>&1; do :; done")
note("a", "A second task. This time the winner gives it back.", 0.5)
run("a", "diavlos send ops --type task 'renew the cert on web-2'", 0.5)
tid = msg_id("a")
together(("a2", "diavlos --as w1 next ops --timeout 60"), ("b", "diavlos --as w2 next ops --timeout 60"))
time.sleep(2)
together(("a2", f"diavlos --as w1 claim ops {tid}"), ("b", f"diavlos --as w2 claim ops {tid}"))
time.sleep(3)
w = winner(tid); l = OTHER[w]
note(w, "Won it, but cannot do it now. Give it back.", 0.5)
run(w, f"diavlos --as {NAME[w]} release ops {tid}", 3.0)
note(l, "Now the other agent can take it.", 0.5)
run(l, f"diavlos --as {NAME[l]} claim ops {tid}", 1.0)
run(l, f"diavlos --as {NAME[l]} send ops --type done --reply-to {tid} 'cert renewed'", 3.0)

note("a", "What the room saw:", 0.5)
run("a", "diavlos read ops | tail -9 | cut -c1-76", 5.0)
note("a", "Done.", 0.5)
note("b", "Done.", 5.0)
log("end")
