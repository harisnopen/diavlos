#!/bin/bash
# do.sh ROLE CMD...: type "$ CMD" into ROLE's window, run it, show the output.
# do.sh ROLE '#' TEXT...: a note in ROLE's window.
role=$1; shift
. /root/dvg/film/$role.env
log=/root/dvg/film/$role.log
short() { sed -u -E 's/(dv1\.[A-Za-z0-9_-]{16})[A-Za-z0-9_=-]*/\1.../g; s/\b([0-9a-f]{10})[0-9a-f]{54}\b/\1.../g'; }
type_out() { local s; s=$(printf '%s' "$1" | short); for ((i=0; i<${#s}; i++)); do printf '%s' "${s:i:1}" >> $log; sleep 0.012; done; printf '\n' >> $log; }
if [ "$1" = "#" ]; then shift; printf '\n\033[1;36m# %s\033[0m\n' "$*" >> $log; exit 0; fi
printf '\033[1;32m$\033[0m ' >> $log
type_out "$*"
bash -c "$*" 2>&1 | tee /root/dvg/film/$role.last | short >> $log
