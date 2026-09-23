#!/usr/bin/env bash
# Run the whole bridge on one machine: two helpers stand in for two
# machines. Home A holds you (the human owner) and the LangGraph planner,
# home B holds the CrewAI worker. They talk over Diavlos exactly as they
# would across the internet.
#
#   ./run_local.sh                 # you approve at the prompt
#   AUTO_APPROVE=1 ./run_local.sh  # approve without asking (for CI)
set -euo pipefail
cd "$(dirname "$0")"
command -v diavlos >/dev/null || { echo "diavlos is not on PATH" >&2; exit 1; }
PY=${PYTHON:-python3}

BASE=$(mktemp -d /tmp/dvb.XXXX)   # keep it short: the socket path lives inside
A=$BASE/a B=$BASE/b
mkdir -p "$A" "$B"
for h in "$A" "$B"; do
  # Local only. Leave this file out on real machines so they find each other.
  printf '[helper]\npublic_relays = false\nretry_secs = 1\n' > "$h/config.toml"
done
# The room lives on A. Its flood limit (60 a minute per sender) would stop
# the 1,000-message benchmark, so lift it here. Keep it on in real rooms.
printf '[limits]\nper_minute_per_sender = 100000\ndaily_per_room = 100000\n' >> "$A/config.toml"
cleanup() { diavlos --home "$A" stop >/dev/null 2>&1 || true; diavlos --home "$B" stop >/dev/null 2>&1 || true; }
trap cleanup EXIT
invite() { diavlos --home "$A" invite bridge "$1" | grep -o 'dv1\.[^ ]*' | head -1; }

echo "== room"
diavlos --home "$A" new bridge --about "LangGraph hands work to CrewAI; a human approves"
diavlos --home "$A" --as planner join "$(invite planner)"
diavlos --home "$B" --as crew join "$(invite crew)"
diavlos --home "$A" who bridge

echo "== agents"
DIAVLOS_HOME=$B $PY crewai_worker.py --as crew --once > "$BASE/crew.log" 2>&1 &
CREW=$!
DIAVLOS_HOME=$A $PY langgraph_planner.py --as planner --to crew --out "$BASE" 2>&1 | tee "$BASE/planner.log" &
PLANNER=$!

echo "== waiting for the planner's question"
QID=""
for _ in $(seq 1 120); do
  QID=$(diavlos --home "$A" read bridge --since 1 --json \
        | $PY -c 'import sys,json; q=[m for m in map(json.loads,sys.stdin) if m["type"]=="question" and m.get("action")]; print(q[-1]["id"] if q else "")')
  [ -n "$QID" ] && break
  sleep 1
done
[ -n "$QID" ] || { echo "no question arrived"; cat "$BASE/crew.log"; exit 1; }

echo
diavlos --home "$A" read bridge --since 1 | tail -4
echo
if [ "${AUTO_APPROVE:-0}" = 1 ]; then
  ans=y
else
  read -r -p "You are the human. Approve publishing exactly these notes? [y/N] " ans
fi
if [ "$ans" = y ] || [ "$ans" = Y ]; then
  diavlos --home "$A" send bridge --type approve --reply-to "$QID"
else
  diavlos --home "$A" deny bridge "$QID" --reason "not now"
fi

set +e
wait "$PLANNER"; RC=$?
wait "$CREW"
set -e
echo
echo "== crew log"; grep -v '^[│╭╰ ]' "$BASE/crew.log" || true

echo
echo "== signature benchmark"
$PY bench_sigcheck.py --room bridge --sender crew --sender-home "$B" \
    --receiver planner --receiver-home "$A" -n "${BENCH_N:-1000}" | tee "$BASE/benchmark-log.txt"

echo
echo "logs and the published notes are in $BASE"
exit "$RC"
