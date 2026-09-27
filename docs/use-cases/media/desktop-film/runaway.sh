#!/bin/bash
# The window on desktop B in the runaway film: agent bot sends a message
# every 2 seconds and shows what came back. It starts when ~/work/go exists.
. /root/dvf/film/b.env; cd $HOME/work
printf '\033[1mbot: one message every 2 seconds\033[0m\n\n'
while [ ! -f go ]; do sleep 0.3; done
n=0
while true; do
  n=$((n + 1))
  out=$(diavlos --as bot send ops "ping $n: all good here" 2>&1); code=$?
  case $code in 0) c='32';; 7) c='1;33';; *) c='1;31';; esac
  printf "\033[${c}mping %-3d exit %d  %s\033[0m\n" $n $code "$(echo "$out" | head -1 | cut -c1-100)"
  sleep 2
done
