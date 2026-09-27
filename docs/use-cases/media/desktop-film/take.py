"""One take of a film on the two desktops.

    python3 take.py ORGO_DIR FILM OUT_DIR

FILM is the name in play-FILM.py (wake, offline, claim, runaway,
notsent, chains or audit). Pushes these scripts to both desktops,
runs play-FILM.py, and, once the room is set up, opens the windows and
records both screens with rec.sh. Then pulls the frames and each window's
log into OUT_DIR. stitch.py makes the video from the frames.
"""
import base64, os, sys, time
HERE = os.path.dirname(os.path.abspath(__file__))
ORGO, FILM, OUT = sys.argv[1], sys.argv[2], os.path.abspath(sys.argv[3])
sys.argv = [sys.argv[0], ORGO]
sys.path.insert(0, HERE)
import film
from film import bash, BOX

LOG = lambda r: f"/root/dvf/film/view-log.sh {r}"
STAGE = {
    "wake": {"a": ("DESKTOP A  desk-a, the room's home", LOG("a")),
             "b": ("DESKTOP B  a shell", LOG("b"), "DESKTOP B  live: worker's wake.log", "/root/dvf/film/wake-view.sh")},
    "offline": {"a": ("DESKTOP A  desk-a, the room's home", LOG("a")),
                "b": ("DESKTOP B  agent reporter", LOG("b"), "DESKTOP B  live: link and outbox", "/root/dvf/film/outbox-loop.sh")},
    "runaway": {"a": ("DESKTOP A  desk-a, the owner", LOG("a")),
                "b": ("DESKTOP B  agent bot", "/root/dvf/film/runaway.sh")},
    "notsent": {"a": ("DESKTOP A  desk-a, the room's home", LOG("a")),
                "b": ("DESKTOP B  agent bot", LOG("b"))},
    "chains": {"a": ("DESKTOP A  desk-a, the owner", LOG("a"), "DESKTOP A  agent a1", LOG("a2")),
               "b": ("DESKTOP B  agents b1, b2, b3", LOG("b"))},
    "audit": {"a": ("DESKTOP A  desk-a, the owner", LOG("a")),
              "b": ("DESKTOP B  agent w2, and the auditor", LOG("b"))},
    "claim": {"a": ("DESKTOP A  desk-a, the room's home", LOG("a"), "DESKTOP A  agent w1", LOG("a2")),
              "b": ("DESKTOP B  agent w2", LOG("b"))},
}[FILM]

for role in ("a", "b"):
    for f in os.listdir(HERE):
        if f.endswith(".sh"):
            b64 = base64.b64encode(open(os.path.join(HERE, f), "rb").read()).decode()
            bash(BOX[role], f"mkdir -p /root/dvf/film && echo {b64} | base64 -d > /root/dvf/film/{f} && chmod +x /root/dvf/film/{f}")

# How far each desktop clock is from ours: the best of five round trips.
off = {}
for role in ("a", "b"):
    s = []
    for _ in range(5):
        t0 = time.time(); r = int(bash(BOX[role], "date +%s%3N")); t1 = time.time()
        s.append((t1 - t0, r - (t0 + t1) / 2 * 1000))
    off[role] = int(min(s)[1])
film.log(f"clock offsets ms {off}")

def start():
    for role in ("a", "b"):
        args = " ".join("'" + x.replace("'", "'\\''") + "'" for x in STAGE[role])
        bash(BOX[role], f"/root/dvf/film/stage.sh {args}")
    for role in ("a", "b"):
        bash(BOX[role], f"rm -rf /root/dvf/frames; nohup /root/dvf/film/rec.sh /root/dvf/frames 600 {off[role]} >/dev/null 2>&1 &")
    time.sleep(4)
    film.log("recording")

film.on_ready = start
exec(open(os.path.join(HERE, f"play-{FILM}.py")).read(), {"__name__": "__main__"})
time.sleep(3)
os.makedirs(OUT, exist_ok=True)
for role in ("a", "b"):
    bash(BOX[role], 'pkill -f "[r]ec.sh /root"; sleep 2; cd /root/dvf && tar czf /tmp/frames.tgz frames')
    open(f"{OUT}/{role}.tgz", "wb").write(base64.b64decode(bash(BOX[role], "base64 -w0 /tmp/frames.tgz", timeout=300)))
    os.makedirs(f"{OUT}/{role}", exist_ok=True)
    os.system(f"tar xzf {OUT}/{role}.tgz -C {OUT}/{role}")
    for name in (role, role + "2"):
        open(f"{OUT}/{name}.log", "w").write(bash(BOX[role], f"cat /root/dvf/film/{name}.log"))
    open(f"{OUT}/{role}-wake.log", "w").write(bash(BOX[role], f"cat /root/dvf/{role}/work/wake.log 2>/dev/null"))
film.log("pulled")
