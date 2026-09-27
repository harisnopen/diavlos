"""Film: what does not get sent. Secrets are refused on the sender's own
machine, and a room set to files = "safe" refuses programs. See film.py."""
from film import *

fresh(); room(b_agents=["bot"])
quiet("b", "printf 'DB_HOST=db.internal\\nAWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\\n' > .env")
quiet("b", "printf '#!/bin/sh\\necho hi\\n' > fix.sh; chmod +x fix.sh; cp /bin/true tool.png")
quiet("b", """python3 -c "
import struct, zlib
def c(t, x): return struct.pack('>I', len(x)) + t + x + struct.pack('>I', zlib.crc32(t + x))
w, h = 64, 32
raw = b''.join(b'\\\\0' + bytes([40, 120 + y * 3, 200]) * w for y in range(h))
open('chart.png', 'wb').write(b'\\\\x89PNG\\\\r\\\\n\\\\x1a\\\\n' + c(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) + c(b'IDAT', zlib.compress(raw)) + c(b'IEND', b''))
" """)
ready()

note("b", "Agent bot, about to make a mistake: send the .env file.", 0.5)
run("b", "cat .env", 1.0)
run("b", "diavlos --as bot send ops 'here is the env' --file .env; echo \"exit $?\"", 3.0)
note("b", "Refused here, on this machine. It never left.", 0.5)
run("b", "diavlos --as bot send ops 'the db password = Tr0ub4dor-and-3-horses-9'; echo \"exit $?\"", 3.0)
run("b", "diavlos --as bot outbox", 2.0)
run("a", "diavlos read ops | tail -2", 3.0)
note("a", "Nothing from bot reached the room.", 3.0)

note("a", "The owner allows only safe files in this room:", 0.5)
run("a", "echo 'files = \"safe\"' >> ~/.diavlos/rooms/ops/policy.toml && diavlos policy ops --show | tail -1", 3.0)
note("b", "A picture is fine.", 0.5)
run("b", "diavlos --as bot send ops 'the chart' --file chart.png", 3.0)
note("b", "A program named .png is not. The home looks at the bytes, not the name.", 0.5)
run("b", "diavlos --as bot send ops 'another chart' --file tool.png; echo \"exit $?\"", 3.0)
run("b", "diavlos --as bot send ops 'a quick fix' --file fix.sh; echo \"exit $?\"", 3.0)
run("a", "diavlos read ops | tail -3", 4.0)
note("a", "Only the picture came in.", 1.0)
note("b", "Done.", 4.0)
log("end")
