#!/bin/bash
# stage.sh ROLE TITLE: one big film window on this desktop, showing ROLE's log.
export DISPLAY=:99
role=$1 title=$2
: > /root/dvf/film/$role.log
printf '\033[1m%s\033[0m\n' "$title" >> /root/dvf/film/$role.log
wmctrl -c "$title" 2>/dev/null; sleep 1
(HOME=/root setsid xfce4-terminal --disable-server --hide-menubar --hide-toolbar --hide-scrollbar \
  --font="DejaVu Sans Mono 11" --title="$title" -e "tail -n +1 -f /root/dvf/film/$role.log" >/dev/null 2>&1 &)
sleep 3
wmctrl -r "$title" -e 0,0,30,1280,660; wmctrl -a "$title"; sleep 1
wmctrl -l | grep -c "$title"
