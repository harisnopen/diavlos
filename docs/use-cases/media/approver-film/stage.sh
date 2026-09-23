#!/bin/bash
# stage.sh "TITLE1" "CMD1" "TITLE2" "CMD2": two film windows, left and right halves.
export DISPLAY=:99 HOME=/root
for t in "THE GATE" "ALICE LAPTOP" "THE AGENTS" "doctor on the agents"; do wmctrl -c "$t" 2>/dev/null; done; sleep 1
open() { (setsid xfce4-terminal --disable-server --hide-menubar --hide-toolbar --hide-scrollbar --font="DejaVu Sans Mono 10" --title="$1" -e "$2" >/dev/null 2>&1 &); }
open "$1" "$2"; open "$3" "$4"; sleep 3
wmctrl -r "$1" -e 0,0,30,636,660; wmctrl -r "$3" -e 0,642,30,636,660
wmctrl -a "$1"; wmctrl -a "$3"; sleep 1
wmctrl -l | grep -E "$1|$3" | wc -l
