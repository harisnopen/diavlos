#!/bin/bash
# The live window on desktop B in the offline film: B's link to the room's
# home, and B's outbox, every second.
. /root/dvf/film/b.env
while true; do
  st=$(diavlos status 2>/dev/null | grep -E '^  ops ' | sed -E 's/ +/ /g; s/^ //')
  out=$(diavlos --as reporter outbox 2>&1)
  clear; printf '\033[1mdiavlos status, outbox\033[0m, checked every second\n\n'
  case "$st" in *offline*) c='1;31';; *) c='1;32';; esac
  printf "\033[${c}m%s\033[0m\n\n" "$st"
  echo "$out" | fold -s -w 76
  sleep 1
done
