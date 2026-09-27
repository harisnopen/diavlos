#!/bin/sh
# wake.sh: the helper runs this when a message waits for "worker".
# Nothing of worker's runs before that. It takes each waiting task,
# does it, says done, and exits.
exec >> "$HOME/work/wake.log" 2>&1
echo "woken: $DIAVLOS_COUNT waiting for $DIAVLOS_AS in $DIAVLOS_ROOM"
get() { printf "%s" "$m" | python3 -c "import json,sys; print(json.load(sys.stdin)[\"$1\"])"; }
while m=$(diavlos --as "$DIAVLOS_AS" next "$DIAVLOS_ROOM" --timeout 3 --json 2>/dev/null); do
  id=$(get id); text=$(get text)
  echo "  task: $text"
  diavlos --as "$DIAVLOS_AS" send "$DIAVLOS_ROOM" --type done --reply-to "$id" "done: $text" >/dev/null
  echo "  said done"
done
echo "nothing left. exit."
