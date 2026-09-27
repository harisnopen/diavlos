"""Film: chains of hand-offs that stop. A task handed back to someone
already in its chain is refused, and so is a chain longer than
max_task_hops. See film.py. Desktop A's right window is agent a1 (a2)."""
from film import *

fresh(); room(b_agents=["b1", "b2", "b3"], a_agents=["a1"])
ready()

note("a", "desk-a owns the room. a1 is an agent here; b1, b2, b3 are on desktop B.", 0.5)
run("a", "diavlos policy ops --show | grep hops", 1.5)
note("a", "A task. Agents hand it on by sending a task in reply.", 0.5)
run("a", "diavlos send ops --type task --to a1 'ship release 2.2'", 1.0)
t0 = msg_id("a")
run("a2", f"diavlos --as a1 send ops --type task --to b1 --reply-to {t0} 'build it'", 3.0)
t1 = msg_id("a2")
run("b", f"diavlos --as b1 send ops --type task --to b2 --reply-to {t1} 'test it'", 3.0)
t2 = msg_id("b")
note("b", "b2 tries to hand it back to a1, who is already in the chain:", 0.5)
run("b", f"diavlos --as b2 send ops --type task --to a1 --reply-to {t2} 'your turn again'; echo \"exit $?\"", 4.0)
note("b", "Refused: that would go round in a circle. b2 hands it on instead.", 0.5)
run("b", f"diavlos --as b2 send ops --type task --to b3 --reply-to {t2} 'package it'", 3.0)
t3 = msg_id("b")
note("b", "b3 wants to pass it on once more:", 0.5)
run("b", f"diavlos --as b3 send ops --type task --to desk-a --reply-to {t3} 'sign it'; echo \"exit $?\"", 4.0)
note("b", "Refused: too many hand-offs. b3 answers instead.", 0.5)
run("b", f"diavlos --as b3 send ops --type done --reply-to {t3} 'packaged: release-2.2.tar.gz'", 3.0)
note("a", "The whole chain, from the first task:", 0.5)
run("a", f"diavlos trace ops {t0} | cut -c1-80", 5.0)
note("a", "Done.", 0.5)
note("b", "Done.", 4.0)
log("end")
