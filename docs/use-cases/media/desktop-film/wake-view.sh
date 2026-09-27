#!/bin/bash
# The live window on desktop B in the wake film: what worker's wake.sh
# wrote, and whether any worker program runs right now.
. /root/dvf/film/b.env
while true; do
  clear; printf '\033[1mwake.log\033[0m, checked every second\n'
  if pgrep -f work/wake.sh >/dev/null; then printf '\033[1;32mwake.sh is running\033[0m\n\n'
  else printf '\033[2mno worker program running\033[0m\n\n'; fi
  [ -f $HOME/work/wake.log ] && grep -v '^error: timed out' $HOME/work/wake.log | tail -30
  sleep 1
done
