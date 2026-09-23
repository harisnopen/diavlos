"""langgraph_planner: a LangGraph agent that hands a job to a CrewAI agent
over Diavlos, then asks a human before it publishes.

    python langgraph_planner.py --room bridge --as planner --to crew

The graph:

    delegate -> wait_for_crew -> ask_human -> publish
                                           \\-> stop   (on a deny)

- delegate: sends a signed `task` with the change list to the CrewAI agent.
- wait_for_crew: waits for its `done`, and checks the Ed25519 signature
  itself before trusting a word of it.
- ask_human: asks the room with a structured action. Only an `approve`
  signed by a human key ends the wait. The action carries the sha256 of the
  exact notes, so the human approves those bytes and nothing else.
- publish: the gate. `diavlos check-approve` must find an unused, unexpired
  human approve for exactly this action, or nothing is published.
"""
import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
from typing import Optional, TypedDict

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "bindings", "python"))
from diavlos import DiavlosError, Room  # noqa: E402
from diavlos.sigcheck import KeyRing  # noqa: E402
from langgraph.graph import END, START, StateGraph  # noqa: E402

CHANGES = [
    "Add dark mode to the settings page",
    "Drop the legacy `sessions_v1` table",
    "Speed up search by 30% with a new index",
    "Rotate the auth token signing secret",
]


def log(*a):
    print(time.strftime("%H:%M:%S"), "[planner]", *a, flush=True)


class State(TypedDict, total=False):
    version: str
    changes: list
    task_id: str
    review: str
    notes: str
    action: dict
    decision: str
    published: Optional[str]


def build(room: Room, keys: KeyRing, room_name: str, me: str, to: str, out_dir: str):
    trace = f"release-{int(time.time())}"

    def delegate(s: State) -> State:
        t = room.send(f"Review the changes for release {s['version']} and flag the risky ones.",
                      type="task", to=to, trace=trace, data={"changes": s["changes"]})
        log(f"delegated to {to}: task {t['id']} (seq {t['seq']})")
        return {"task_id": t["id"]}

    def wait_for_crew(s: State) -> State:
        while True:
            try:
                m = room.next_one(timeout=300)
            except DiavlosError as e:
                if e.code == 4:
                    raise SystemExit(f"{to} did not answer in 300 s")
                raise
            ok, us = keys.check(m)
            log(f"sigcheck {m['from']} #{m['seq']} {m['type']}: {'ok' if ok else 'BAD'} in {us:.0f} us")
            if not ok:
                log("ignoring a message whose signature does not verify")
                continue
            if m.get("reply_to") != s["task_id"]:
                continue
            if m["type"] == "claim":
                log(f"{m['from']} claimed the task")
            elif m["type"] == "done" and m["from"] == to:
                log(f"{to} finished: {m['text'][:100]!r}")
                return {"review": m["text"]}
            elif m["type"] == "reply":
                raise SystemExit(f"{to} could not do it: {m['text']}")

    def ask_human(s: State) -> State:
        notes = f"# Release {s['version']}\n\n" + "\n".join(f"- {c}" for c in s["changes"])
        notes += f"\n\nReview by {to}: {s['review']}\n"
        digest = hashlib.sha256(notes.encode()).hexdigest()
        action = {"verb": "publish", "target": "release-notes",
                  "params": {"version": s["version"], "sha256": digest}}
        log(f"asking a human to approve publishing notes sha256:{digest[:16]}...")
        log("  approve from the CLI with a human key:  diavlos send "
            f"{room_name} --type approve --reply-to <question id>   (see: diavlos read {room_name})")
        try:
            reply = room.ask(f"Publish release notes {s['version']}? Review said: {s['review'][:200]}",
                             timeout=600, action=action, trace=trace)
        except DiavlosError as e:
            if e.code == 6:
                log(f"denied: {e.message}")
                return {"notes": notes, "action": action, "decision": "deny"}
            raise
        ok, us = keys.check(reply)
        log(f"sigcheck {reply['from']} #{reply['seq']} {reply['type']}: {'ok' if ok else 'BAD'} in {us:.0f} us")
        decision = reply["type"] if ok else "deny"
        log(f"human answer: {decision} from {reply['from']}")
        return {"notes": notes, "action": action, "decision": decision}

    def publish(s: State) -> State:
        gate = subprocess.run(["diavlos", "--as", me, "check-approve", room_name, json.dumps(s["action"])],
                              capture_output=True, text=True)
        log("gate:", (gate.stdout or gate.stderr).strip(), f"(exit {gate.returncode})")
        if gate.returncode != 0:
            return {"published": None}
        path = os.path.join(out_dir, f"release-{s['version']}.md")
        with open(path, "w") as f:
            f.write(s["notes"])
        room.send(f"published {path}", type="done", trace=trace)
        log("published", path)
        return {"published": path}

    def stop(s: State) -> State:
        log("not publishing")
        return {"published": None}

    g = StateGraph(State)
    for name, fn in [("delegate", delegate), ("wait_for_crew", wait_for_crew),
                     ("ask_human", ask_human), ("publish", publish), ("stop", stop)]:
        g.add_node(name, fn)
    g.add_edge(START, "delegate")
    g.add_edge("delegate", "wait_for_crew")
    g.add_edge("wait_for_crew", "ask_human")
    g.add_conditional_edges("ask_human", lambda s: "publish" if s["decision"] == "approve" else "stop")
    g.add_edge("publish", END)
    g.add_edge("stop", END)
    return g.compile()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--room", default=os.environ.get("BRIDGE_ROOM", "bridge"))
    ap.add_argument("--as", dest="me", default=os.environ.get("BRIDGE_ME", "planner"))
    ap.add_argument("--to", default=os.environ.get("BRIDGE_TO", "crew"))
    ap.add_argument("--version", default="2.4.0")
    ap.add_argument("--out", default=".")
    args = ap.parse_args()

    room = Room.open(args.room, name=args.me)
    keys = KeyRing.load(args.room, name=args.me)
    members = room.who()
    log("members:", ", ".join(f"{w['name']}({w['kind']}/{w['fingerprint']})" for w in members))
    for w in members:
        if keys.fingerprints.get(w["name"]) != w["fingerprint"]:
            raise SystemExit(f"key for {w['name']} does not match `who`: stopping")
    graph = build(room, keys, args.room, args.me, args.to, args.out)
    final = graph.invoke({"version": args.version, "changes": CHANGES})
    sys.exit(0 if final.get("published") else 6)


if __name__ == "__main__":
    main()
