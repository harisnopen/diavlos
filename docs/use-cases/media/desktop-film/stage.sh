#!/bin/bash
# stage.sh TITLE CMD [TITLE2 CMD2]: film windows on this desktop. One window
# fills the screen; two take the left and right halves. Titles start with
# "DESKTOP ", so the windows of an earlier take can be found and closed.
export DISPLAY=:99
wmctrl -l | grep -o 'DESKTOP .*' | while read -r t; do wmctrl -c "$t"; done; sleep 1
open() { (HOME=/root setsid xfce4-terminal --disable-server --hide-menubar --hide-toolbar --hide-scrollbar \
  --font="DejaVu Sans Mono 11" --title="$1" -e "$2" >/dev/null 2>&1 &); }
open "$1" "$2"; [ -n "$3" ] && open "$3" "$4"; sleep 3
if [ -n "$3" ]; then
  wmctrl -r "$1" -e 0,0,30,637,660; wmctrl -r "$3" -e 0,643,30,637,660; wmctrl -a "$1"; wmctrl -a "$3"
else
  wmctrl -r "$1" -e 0,0,30,1280,660; wmctrl -a "$1"
fi
sleep 1; wmctrl -l | grep -c 'DESKTOP '
