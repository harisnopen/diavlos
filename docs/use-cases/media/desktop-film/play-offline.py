"""Film: the room's home goes away and comes back; nothing sent while it
was away is lost. See film.py. B's right-hand window is outbox-loop.sh."""
from film import *

fresh(); room(b_agents=["reporter"])
quiet("b", "printf 'disk 71%% used\\nload 0.42\\n' > health.txt")
ready()

note("a", "Desktop A: desk-a, the room's home. reporter is an agent on desktop B.", 0.5)
run("a", "diavlos read ops", 2.0)
note("a", "The home goes away: stop its helper.", 0.5)
run("a", "diavlos stop", 4.0)

note("b", "B keeps working. It sends three reports and a file.", 0.5)
run("b", "diavlos --as reporter send ops 'report 1: all good'", 0.5)
run("b", "diavlos --as reporter send ops 'report 2: disk at 71%'", 0.5)
run("b", "diavlos --as reporter send ops 'report 3: health file' --file health.txt", 1.0)
note("b", "Nothing is lost. They wait in the outbox, signed.", 8.0)

note("a", "The home is back. Any command starts the helper.", 0.5)
run("a", "diavlos status", 6.0)
note("b", "B saw the home come back. The outbox is empty.", 1.0)
run("a", "diavlos read ops", 1.5)
fid = re.search(r"sha256:([0-9a-f]{12})", last("a")).group(1)
note("a", "All three, in order. The file came too:", 0.5)
run("a", f"diavlos get ops {fid} && cat ~/.diavlos/files/ops/health.txt", 5.0)

note("a", "Done.", 0.5)
note("b", "Done.", 5.0)
log("end")
