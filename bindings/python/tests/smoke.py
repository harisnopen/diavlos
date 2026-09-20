"""Smoke test: a home-made Python bot joins a room made by the CLI and
finishes a task. Needs the diavlos binary (DIAVLOS_BIN or on PATH)."""
import os
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from diavlos import Room, DiavlosError  # noqa: E402

BIN = os.environ.get("DIAVLOS_BIN", "diavlos")


def cli(home, *args, user="haris"):
    env = dict(os.environ, DIAVLOS_HOME=home, USER=user, USERNAME=user)
    out = subprocess.run([BIN, *args], env=env, capture_output=True, text=True)
    if out.returncode != 0:
        raise SystemExit(f"cli {args} failed: {out.stderr}")
    return out.stdout


def main():
    base = tempfile.mkdtemp(prefix="diavlos-py-")
    a, b = os.path.join(base, "a"), os.path.join(base, "b")
    for h in (a, b):
        os.makedirs(h)
        with open(os.path.join(h, "config.toml"), "w") as f:
            f.write("[helper]\npublic_relays = false\nretry_secs = 1\n")
    try:
        cli(a, "new", "ops")
        note = cli(a, "invite", "ops", "pybot")
        invite = next(w for w in note.split() if w.startswith("dv1."))

        room = Room.join(invite, name="pybot", home=b)
        assert room.me == "pybot", room.me
        m = room.send("hello from python")
        assert m["from"] == "pybot" and m["seq"] >= 3, m

        got = cli(a, "next", "ops", "--timeout", "20")
        assert "pybot (chat): hello from python" in got, got

        cli(a, "send", "ops", "please do x", "--type", "task")
        for msg in room.next(timeout=20):
            if msg.type == "task":
                done = room.send("did x", type="done", reply_to=msg.id)
                assert done.reply_to == msg.id
                break

        who = room.who()
        assert {w["name"] for w in who} == {"haris", "pybot"}, who
        assert all(len(w["fingerprint"]) == 16 for w in who)

        try:
            room.next_one(timeout=1)
            raise SystemExit("expected a timeout")
        except DiavlosError as e:
            assert e.code == 4, e

        try:
            Room.open("nope", name="pybot", home=b).send("x")
            raise SystemExit("expected not-in-room")
        except DiavlosError as e:
            assert e.code == 2, e

        log = room.read(since=1, limit=100)
        assert any(x.text == "did x" for x in log)
        print("python smoke: ok")
    finally:
        for h in (a, b):
            subprocess.run([BIN, "--home", h, "stop"], capture_output=True)
        time.sleep(0.3)


if __name__ == "__main__":
    main()
