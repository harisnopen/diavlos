#!/bin/bash
# rec.sh DIR SECONDS OFFSET_MS: a screenshot of this desktop every 1.5 s.
# The two desktop clocks were seconds apart, so play.py measures each one's
# offset from its own clock and passes it in. Shots land on the same 1.5 s
# beat of that shared clock on both desktops, named by their beat.
export DISPLAY=:99
dir=$1 end=$(( $(date +%s) + $2 )) off=$3; mkdir -p $dir
while [ $(date +%s) -lt $end ]; do
  now=$(( $(date +%s%3N) - off )); next=$(( (now / 1500 + 1) * 1500 ))
  sleep $(awk "BEGIN{print ($next-$now)/1000}")
  scrot -o $dir/$next.png &
done
wait
