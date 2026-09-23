"""bench_sigcheck: how long does checking a Diavlos signature take?

    python bench_sigcheck.py --room bridge --sender crew --sender-home B \\
        --receiver planner --receiver-home A -n 1000

The sender posts N signed chat messages. The receiver reads them from its
own helper and verifies every Ed25519 signature in Python (SPEC 2.3),
timing only the check. Prints p50, p99 and max, plus the full `diavlos
verify` of an export (every signature and the whole hash chain).
"""
import argparse
import os
import platform
import statistics
import subprocess
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "bindings", "python"))
from diavlos import Room  # noqa: E402
from diavlos.sigcheck import KeyRing  # noqa: E402


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(round(p / 100 * (len(xs) - 1))))]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--room", default="bridge")
    ap.add_argument("--sender", default="crew")
    ap.add_argument("--sender-home")
    ap.add_argument("--receiver", default="planner")
    ap.add_argument("--receiver-home")
    ap.add_argument("-n", type=int, default=1000)
    args = ap.parse_args()

    tx = Room.open(args.room, name=args.sender, home=args.sender_home)
    rx = Room.open(args.room, name=args.receiver, home=args.receiver_home)
    first = None
    for i in range(args.n):
        m = tx.send(f"bench message {i}", trace="sigbench", data={"i": i, "pad": "x" * 200})
        first = first or m["seq"]
    last = m["seq"]

    seen, deadline = {}, time.time() + 120
    while len(seen) < args.n and time.time() < deadline:
        since = max(seen, default=first - 1) + 1
        batch = rx.read(since=since, limit=500)
        seen.update({x["seq"]: x for x in batch if first <= x["seq"] <= last})
        if not batch:
            time.sleep(0.2)
    got = [seen[k] for k in sorted(seen)]
    if len(got) < args.n:
        raise SystemExit(f"only {len(got)} of {args.n} arrived")

    keys = KeyRing.load(args.room, name=args.receiver, home=args.receiver_home)
    times, bad = [], 0
    for msg in got:
        ok, us = keys.check(msg)
        bad += not ok
        times.append(us)

    home = ["--home", args.receiver_home] if args.receiver_home else []
    exe = os.environ.get("DIAVLOS_BIN", "diavlos")
    bundle = subprocess.run([exe, *home, "--as", args.receiver, "export", args.room],
                            capture_output=True, text=True, check=True).stdout
    total = bundle.count("\n") - 1
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), ".bench.bundle.jsonl")
    with open(path, "w") as f:
        f.write(bundle)
    t0 = time.perf_counter()
    v = subprocess.run([exe, "verify", path], capture_output=True, text=True)
    verify_ms = (time.perf_counter() - t0) * 1000
    os.remove(path)

    print(f"# Diavlos signature check, {time.strftime('%Y-%m-%d %H:%M:%S %Z')}")
    print(f"# {platform.platform()}, {platform.processor() or platform.machine()}, Python {platform.python_version()}")
    print(f"messages checked      {len(times)}  (bad signatures: {bad})")
    print(f"per message, p50      {statistics.median(times):8.1f} us")
    print(f"per message, p99      {pct(times, 99):8.1f} us")
    print(f"per message, max      {max(times):8.1f} us   ({max(times) / 1000:.3f} ms, budget 10 ms)")
    print(f"per message, mean     {statistics.mean(times):8.1f} us")
    print(f"diavlos verify, whole bundle of {total} messages (signatures + chain, incl. process start): "
          f"{verify_ms:.1f} ms, exit {v.returncode}")
    sys.exit(0 if bad == 0 and max(times) < 10_000 and v.returncode == 0 else 1)


if __name__ == "__main__":
    main()
