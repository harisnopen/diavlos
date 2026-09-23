#!/bin/bash
# Live doctor on the agents' machine: only the lines about keys, the room and approvals.
. /root/dvg/film/agents.env
while true; do
  out=$(diavlos doctor 2>/dev/null | grep -E '^(ok  |FAIL|WARN) (keys|room|approvals)' | grep -v 'none yet' | sed -E 's/  +/  /')
  clear; printf '\033[1mdiavlos doctor\033[0m, every 2 s        %s UTC\n\n' "$(date -u +%T)"
  if [ -z "$out" ]; then echo "(helper not started yet)"; else echo "$out" | fold -s -w 74 | sed -E 's/^(WARN.*)/\x1b[1;33m\1\x1b[0m/'; fi
  echo "$out" | grep -q WARN || printf '\n\033[1;32mno WARN: no key that can say yes is on this machine\033[0m\n'
  sleep 2
done
