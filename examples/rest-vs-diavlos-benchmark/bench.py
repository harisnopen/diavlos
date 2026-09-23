"""REST/JSON webhooks against Diavlos typed messages, LangChain to CrewAI.

    python bench.py              # 1,000 messages per transport
    python bench.py -n 200       # quicker

A LangChain tool hands a task to a CrewAI tool, which checks it and
answers. The same two tools run over three transports:

  rest       a plain JSON POST to a local webhook. The receiver checks the
             shape of the payload. This is what most hand-offs are today.
  rest+hmac  the same, plus an HMAC-SHA256 header over the body with a
             secret both sides share.
  diavlos    a typed `task` into a Diavlos room, answered with a `done`.
             Every message is Ed25519-signed by the sender's own key and
             written to disk before `send` returns; the receiver checks the
             signature again in Python.

Then three attacks and one outage, run for real against each transport:
a changed payload, a stranger posing as the sender, a member posing as
another member, and the receiver being offline.

Everything runs on this machine: two Diavlos helpers stand in for two
machines. Numbers go to stdout and to results.md.
"""
import argparse
import hashlib
import hmac
import http.client
import json
import os
import platform
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

os.environ.setdefault("CREWAI_DISABLE_TELEMETRY", "true")
os.environ.setdefault("OTEL_SDK_DISABLED", "true")
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "bindings", "python"))
from diavlos import DiavlosError, Room  # noqa: E402
from diavlos.sigcheck import KeyRing, signed_bytes  # noqa: E402
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey  # noqa: E402

BIN = os.environ.get("DIAVLOS_BIN") or shutil.which("diavlos") or "diavlos"
SECRET = b"shared-secret-both-sides-know"


# -- the two framework tools -------------------------------------------------

def make_langchain_tool(transport_send):
    """The sending side: a LangChain tool. Falls back to a plain function
    if langchain-core is not installed, and says so in the results."""
    try:
        from langchain_core.tools import StructuredTool
    except ImportError:
        return transport_send, "plain function (langchain-core not installed)"

    def hand_off(task: str, changes: list, i: int) -> dict:
        """Hand a review task to the CrewAI side and return its answer."""
        return transport_send({"task": task, "changes": changes, "i": i})

    return StructuredTool.from_function(hand_off).invoke, "langchain_core StructuredTool"


def make_crewai_tool():
    """The receiving side: a CrewAI tool that does the (tiny) work."""
    def review(changes):
        return [c for c in changes if "drop" in c.lower()]
    try:
        from crewai.tools import BaseTool
    except ImportError:
        return lambda **kw: review(kw["changes"]), "plain function (crewai not installed)"

    class ReviewTool(BaseTool):
        name: str = "review_changes"
        description: str = "Flag risky changes in a list."

        def _run(self, changes: list) -> list:
            return review(changes)

    tool = ReviewTool()
    return lambda **kw: tool.run(**kw), "crewai BaseTool"


def valid_shape(p):
    """The schema check every receiver does."""
    return (isinstance(p, dict) and isinstance(p.get("task"), str)
            and isinstance(p.get("changes"), list)
            and all(isinstance(c, str) for c in p["changes"])
            and isinstance(p.get("i"), int))


def payload(i):
    return {"task": f"review release {i}", "i": i,
            "changes": ["Add dark mode", "Drop the legacy table", f"Bump build {i}"]}


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(round(p / 100 * (len(xs) - 1))))]


# -- REST ----------------------------------------------------------------------

class Webhook:
    """A CrewAI tool behind a JSON webhook, with or without HMAC."""

    def __init__(self, crew_tool, use_hmac):
        self.validate_us, self.accepted, self.rejected = [], 0, 0
        outer = self

        class H(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"
            disable_nagle_algorithm = True   # else every reply waits ~40 ms on delayed ACK

            def log_message(self, *a):
                pass

            def do_POST(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                t0 = time.perf_counter()
                ok = True
                if use_hmac:
                    want = hmac.new(SECRET, body, hashlib.sha256).hexdigest()
                    ok = hmac.compare_digest(want, self.headers.get("X-Signature", ""))
                try:
                    p = json.loads(body)
                except ValueError:
                    p = None
                ok = ok and valid_shape(p)
                outer.validate_us.append((time.perf_counter() - t0) * 1e6)
                if not ok:
                    outer.rejected += 1
                    out, code = b'{"error":"rejected"}', 400
                else:
                    outer.accepted += 1
                    out = json.dumps({"type": "done", "reply_to": p["i"],
                                      "risky": crew_tool(changes=p["changes"])}).encode()
                    code = 200
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(out)))
                self.end_headers()
                self.wfile.write(out)

        self.srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
        self.port = self.srv.server_address[1]
        self.thread = threading.Thread(target=self.srv.serve_forever, daemon=True)
        self.thread.start()

    def stop(self):
        self.srv.shutdown()
        self.srv.server_close()


def connect(port):
    """A keep-alive connection with Nagle off, so REST is not slowed by a
    TCP artefact that has nothing to do with REST."""
    conn = http.client.HTTPConnection("127.0.0.1", port)
    conn.connect()
    conn.sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    return conn


def rest_client(port, use_hmac):
    conn = connect(port)

    def post(p, sign=True, raw=None):
        body = raw if raw is not None else json.dumps(p).encode()
        h = {"Content-Type": "application/json"}
        if use_hmac and sign:
            h["X-Signature"] = hmac.new(SECRET, body, hashlib.sha256).hexdigest()
        conn.request("POST", "/hook", body=body, headers=h)
        r = conn.getresponse()
        return r.status, json.loads(r.read())
    return post


def run_rest(n, use_hmac, crew_tool):
    hook = Webhook(crew_tool, use_hmac)
    post = rest_client(hook.port, use_hmac)
    send = lambda p: post(p)[1]  # noqa: E731
    lc, lc_kind = make_langchain_tool(send)
    rtt = []
    t_all = time.perf_counter()
    for i in range(n):
        t0 = time.perf_counter()
        ans = lc({"task": f"review release {i}", "changes": payload(i)["changes"], "i": i})
        rtt.append((time.perf_counter() - t0) * 1000)
        assert ans["reply_to"] == i
    wall = time.perf_counter() - t_all

    # attack 1: someone on the path changes the payload (keeps the old HMAC)
    body = json.dumps(payload(1)).encode()
    sig = hmac.new(SECRET, body, hashlib.sha256).hexdigest()
    evil = body.replace(b"Drop the legacy table", b"Nothing risky here   ")
    conn = connect(hook.port)
    conn.request("POST", "/hook", body=evil, headers={"Content-Type": "application/json", "X-Signature": sig})
    tamper_accepted = conn.getresponse().status == 200
    # attack 2: a stranger with no secret posts as the planner.
    status, _ = post(payload(2), sign=False)
    stranger_accepted = status == 200
    # attack 3: an insider (the crew, which holds the same secret) posts as
    # the planner. A shared secret cannot tell the two apart.
    status, _ = post(payload(3), sign=True)
    insider_accepted = status == 200
    # outage: the receiver is down for a moment
    hook.stop()
    lost = 0
    for i in range(10):
        try:
            rest_client(hook.port, use_hmac)(payload(i))
        except OSError:
            lost += 1
    return dict(rtt=rtt, wall=wall, validate=hook.validate_us, lc=lc_kind,
                tamper_accepted=tamper_accepted, stranger_accepted=stranger_accepted,
                insider_accepted=insider_accepted,
                outage_lost=lost)


# -- Diavlos -------------------------------------------------------------------

def cli(home, *args):
    return subprocess.run([BIN, "--home", home, *args], capture_output=True, text=True, check=True).stdout


def setup_room(base):
    a, b = os.path.join(base, "a"), os.path.join(base, "b")
    for h in (a, b):
        os.makedirs(h)
        with open(os.path.join(h, "config.toml"), "w") as f:
            f.write("[helper]\npublic_relays = false\nretry_secs = 1\n")
    with open(os.path.join(a, "config.toml"), "a") as f:
        # The room's flood limit would stop a 1,000-message benchmark.
        f.write("[limits]\nper_minute_per_sender = 1000000\ndaily_per_room = 1000000\n")
    cli(a, "new", "bench")
    inv = lambda who: next(w for w in cli(a, "invite", "bench", who).split() if w.startswith("dv1."))  # noqa: E731
    cli(a, "--as", "planner", "join", inv("planner"))
    cli(b, "--as", "crew", "join", inv("crew"))
    return a, b


def run_diavlos(n, crew_tool, base):
    a, b = setup_room(base)
    tx = Room.open("bench", name="planner", home=a)
    rx = Room.open("bench", name="crew", home=b)
    rx_keys = KeyRing.load("bench", name="crew", home=b)
    tx_keys = KeyRing.load("bench", name="planner", home=a)
    validate_us, stop = [], threading.Event()

    def crew_loop():
        while not stop.is_set():
            try:
                m = rx.next_one(timeout=1)
            except DiavlosError as e:
                if e.code == 4:
                    continue
                raise
            if m["type"] != "task":
                continue
            t0 = time.perf_counter()
            ok, _ = rx_keys.check(m)
            ok = ok and valid_shape(m.get("data"))
            validate_us.append((time.perf_counter() - t0) * 1e6)
            if ok:
                rx.send("reviewed", type="done", reply_to=m["id"],
                        data={"risky": crew_tool(changes=m["data"]["changes"]), "i": m["data"]["i"]})

    worker = threading.Thread(target=crew_loop, daemon=True)
    worker.start()

    def send(p):
        t = tx.send(p["task"], type="task", to="crew", data=p)
        while True:
            m = tx.next_one(timeout=30)
            if m["type"] == "done" and m.get("reply_to") == t["id"]:
                ok, _ = tx_keys.check(m)
                assert ok, "reply signature did not verify"
                return m["data"]

    lc, lc_kind = make_langchain_tool(send)
    rtt = []
    t_all = time.perf_counter()
    for i in range(n):
        t0 = time.perf_counter()
        ans = lc({"task": f"review release {i}", "changes": payload(i)["changes"], "i": i})
        rtt.append((time.perf_counter() - t0) * 1000)
        assert ans["i"] == i
    wall = time.perf_counter() - t_all
    stop.set()
    worker.join()

    # attack 1: change the payload of a real signed task on its way in
    real = tx.send("review release x", type="task", to="nobody", data=payload(1))
    evil = dict(real, data=dict(payload(1), changes=["Nothing risky here"]))
    tamper_accepted = rx_keys.check(evil)[0]
    # attack 2: a stranger signs a task as the planner with a key of its own.
    stranger = dict(real, id="m_forged", **{"from": "planner"})
    stranger["sig"] = "ed25519:" + Ed25519PrivateKey.generate().sign(signed_bytes(stranger)).hex()
    stranger_accepted = rx_keys.check(stranger)[0]
    # attack 3: an insider (the crew, a real member) signs a message with its
    # own key and relabels it as the planner's.
    insider = dict(rx.send("review release y", type="chat", data=payload(3)), **{"from": "planner"})
    insider_accepted = rx_keys.check(insider)[0]
    # outage: stop the crew's helper, send 10 tasks, start it again
    cli(b, "stop")
    time.sleep(0.5)
    ids = {tx.send(f"while you were out {i}", type="task", to="crew", data=payload(i))["id"] for i in range(10)}
    got, deadline = set(), time.time() + 60
    while ids - got and time.time() < deadline:
        try:
            m = rx.next_one(timeout=5)   # first call restarts the helper
        except DiavlosError as e:
            if e.code == 4:
                continue
            raise
        if m["id"] in ids and rx_keys.check(m)[0]:
            got.add(m["id"])
    for h in (a, b):
        subprocess.run([BIN, "--home", h, "stop"], capture_output=True)
    return dict(rtt=rtt, wall=wall, validate=validate_us, lc=lc_kind,
                tamper_accepted=tamper_accepted, stranger_accepted=stranger_accepted,
                insider_accepted=insider_accepted,
                outage_lost=len(ids - got))


# -- report --------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-n", type=int, default=1000)
    ap.add_argument("--out", default=os.path.join(HERE, "results.md"))
    args = ap.parse_args()

    crew_tool, crew_kind = make_crewai_tool()
    base = tempfile.mkdtemp(prefix="dvbm-", dir="/tmp" if os.path.isdir("/tmp") else None)
    runs = {}
    try:
        for name, fn in [("rest", lambda: run_rest(args.n, False, crew_tool)),
                         ("rest+hmac", lambda: run_rest(args.n, True, crew_tool)),
                         ("diavlos", lambda: run_diavlos(args.n, crew_tool, base))]:
            print(f"running {name} x {args.n} ...", flush=True)
            runs[name] = fn()
    finally:
        shutil.rmtree(base, ignore_errors=True)

    yes = lambda b: "accepted" if b else "rejected"  # noqa: E731
    lines = [
        f"# REST/JSON vs Diavlos, {args.n} hand-offs each",
        "",
        f"Run {time.strftime('%Y-%m-%d %H:%M %Z')} on {platform.platform()}, Python {platform.python_version()}, "
        f"one machine, two Diavlos helpers. Sender: {runs['rest']['lc']}. Receiver: {crew_kind}.",
        "",
        "## Speed",
        "",
        "| transport | total | per hand-off p50 | p99 | receiver check p50 | receiver check p99 |",
        "|---|---|---|---|---|---|",
    ]
    for name, r in runs.items():
        lines.append(f"| {name} | {r['wall']:.2f} s | {statistics.median(r['rtt']):.2f} ms | "
                     f"{pct(r['rtt'], 99):.2f} ms | {statistics.median(r['validate']):.1f} us | "
                     f"{pct(r['validate'], 99):.1f} us |")
    lines += [
        "",
        "A hand-off is a full round trip: the LangChain tool sends a task, the CrewAI tool",
        "checks it and answers, and the sender has the answer. The receiver check is",
        "the shape check, plus the HMAC or the Ed25519 signature where there is one.",
        "",
        "## Attacks and outages (run for real, not assumed)",
        "",
        "| test | rest | rest+hmac | diavlos |",
        "|---|---|---|---|",
        "| payload changed on the way | " + " | ".join(yes(runs[k]["tamper_accepted"]) for k in runs) + " |",
        "| stranger claims to be the planner | "
        + " | ".join(yes(runs[k]["stranger_accepted"]) for k in runs) + " |",
        "| another member claims to be the planner | "
        + " | ".join(yes(runs[k]["insider_accepted"]) for k in runs) + " |",
        "| receiver offline, 10 hand-offs sent | "
        + " | ".join(f"{runs[k]['outage_lost']} of 10 lost" for k in runs) + " |",
        "",
        "## What the numbers say",
        "",
        "- Plain REST is the fastest by far and checks nothing but the shape.",
        "- HMAC stops a changed payload and a stranger, but every holder of the",
        "  shared secret can sign as anyone, so it cannot tell the planner from the crew.",
        "- Diavlos costs more per hand-off: each message is signed, chained,",
        "  written to disk before `send` returns, and routed through two helpers.",
        "  For that it rejects changed and forged messages by key, and loses",
        "  nothing while the receiver is down.",
    ]
    report = "\n".join(lines) + "\n"
    print()
    print(report)
    with open(args.out, "w") as f:
        f.write(report)
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
