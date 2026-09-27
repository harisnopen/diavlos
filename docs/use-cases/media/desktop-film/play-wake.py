"""Film docs/WAKE.md: an agent that is not running is woken when a task
waits for it. See film.py. B's right-hand window is wake-view.sh."""
from film import *

fresh(); room(b_agents=["worker"])
quiet("b", "cp /root/dvf/film/wake.sh . && touch wake.log")
ready()

note("a", "Desktop A: desk-a, the room's home. worker is an agent on desktop B.", 0.5)
run("a", "diavlos who ops | cut -c1-62", 1.0)
note("b", "Nothing of worker's is running here. This script is all it is:", 0.5)
run("b", "cat wake.sh", 2.0)
note("b", "One rule: when a message waits for worker, the helper runs wake.sh.", 0.5)
run("b", "diavlos wake add ops --as worker --exec ~/work/wake.sh", 1.0)
run("b", "diavlos wake list", 3.0)

note("a", "Send worker a task. Nothing on B is running to read it.", 0.5)
run("a", "diavlos send ops --type task --to worker 'rotate the logs on web-1'", 0.5)
run("a", "diavlos next ops --timeout 60", 5.0)
note("b", "The helper woke wake.sh. It did the task, said done, and exited.", 5.0)

note("a", "Three tasks at once. One wake-up takes all three.", 0.5)
run("a", "for t in 'back up the db' 'renew the cert' 'clear the cache'; do diavlos send ops --type task --to worker \"$t\"; done", 8.0)
run("a", "diavlos read ops | tail -6", 4.0)
note("b", "Woken once, with count 3. Now nothing runs again.", 4.0)

note("a", "Done. The guide is docs/WAKE.md.", 0.5)
note("b", "Done. The guide is docs/WAKE.md.", 5.0)
log("end")
