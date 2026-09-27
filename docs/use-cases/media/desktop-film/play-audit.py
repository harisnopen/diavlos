"""Film: an audit you can check. The owner exports the room; an observer
on the other desktop verifies it, then changes one word and sees it
caught. See film.py."""
from film import *

fresh(); room(b_agents=["w2", "auditor:observer"])
ready()

note("a", "desk-a owns the room. Some work happens.", 0.5)
run("a", "diavlos send ops --type task 'rotate the logs on web-1'", 1.0)
t = msg_id("a")
run("b", f"diavlos --as w2 claim ops {t}", 1.0)
run("b", f"diavlos --as w2 send ops --type done --reply-to {t} 'logs rotated'", 3.0)
note("a", "Export the room: every message, signed, in one file.", 0.5)
run("a", "diavlos export ops > ops.bundle && diavlos verify ops.bundle", 2.0)
note("a", "Hand it to the auditor on desktop B.", 0.5)
run("a", "diavlos send ops 'audit bundle for today' --file ops.bundle", 2.0)
fid = re.search(r"sha256:([0-9a-f]{12})", last("a")).group(1)

note("b", "The auditor: a read-only key. First, the owner key it should be:", 0.5)
run("b", "diavlos --as auditor who ops | head -2", 1.5)
owner = re.search(r"desk-a\s+\S+\s+\S+\s+([0-9a-f]{16})", last("b")).group(1)
run("b", f"diavlos --as auditor get ops {fid} && cp ~/.diavlos/files/ops/ops.bundle .", 1.0)
run("b", f"diavlos verify ops.bundle --owner {owner}", 4.0)
note("b", "Now change one word in the log, as a cover-up would:", 0.5)
run("b", "sed -i 's/logs rotated/logs deleted/' ops.bundle && diavlos verify ops.bundle; echo \"exit $?\"", 4.0)
note("b", "Caught. Now expect a different owner key, as if the bundle were from someone else:", 0.5)
run("b", "cp ~/.diavlos/files/ops/ops.bundle . && diavlos verify ops.bundle --owner 0123456789abcdef; echo \"exit $?\"", 4.0)
note("b", "Caught too. verify needs no helper and no network.", 1.0)
note("a", "Done.", 4.0)
log("end")
