"""Film: stop a runaway agent. The owner pauses the room, resumes it,
mutes the agent and then revokes it. See film.py.
Desktop B's window is runaway.sh: agent bot sends every 2 seconds."""
from film import *

fresh(); room(b_agents=["bot"])
ready()

note("a", "desk-a owns the room. On desktop B, agent bot sends a message every 2 s.", 0.5)
quiet("b", "touch go")
time.sleep(5)
run("a", "diavlos read ops | tail -3", 3.0)

note("a", "1. Kill switch: pause the whole room. Nothing moves.", 0.5)
run("a", "diavlos pause ops", 8.0)
run("a", "diavlos send ops 'even the owner waits'", 3.0)
note("a", "Resume. Messages flow again.", 0.5)
run("a", "diavlos resume ops", 7.0)

note("a", "2. Mute only bot. Everyone else can still talk.", 0.5)
run("a", "diavlos mute ops bot", 7.0)
run("a", "diavlos send ops 'the rest of the room still works'", 3.0)

note("a", "3. Revoke bot's key for good.", 0.5)
run("a", "diavlos revoke ops bot", 8.0)
run("a", "diavlos who ops | cut -c1-62", 1.0)
run("a", "diavlos read ops | tail -8 | cut -c1-80", 5.0)
note("a", "bot's key is cut. It can never send here again.", 4.0)
log("end")
