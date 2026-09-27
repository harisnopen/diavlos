#!/bin/bash
# do.sh ROLE CMD...: type "$ CMD" into ROLE's window, run it, show the output.
# do.sh ROLE '#' TEXT...: a note in ROLE's window.
role=$1; shift
. /root/dvf/film/$role.env
log=/root/dvf/film/$role.log
# On screen only: $HOME as ~, invite tokens and node ids cut short.
# File fingerprints are shown in full.
short() { sed -u -E "s#$HOME#~#g; s/(dv1\.[A-Za-z0-9_-]{16})[A-Za-z0-9_=-]*/\1.../g; s/((for|node) [0-9a-f]{10})[0-9a-f]{54}/\1.../g"; }
type_out() { local s; s=$(printf '%s' "$1" | short); for ((i=0; i<${#s}; i++)); do printf '%s' "${s:i:1}" >> $log; sleep 0.02; done; printf '\n' >> $log; }
if [ "$1" = "#" ]; then shift; printf '\n\033[1;36m# %s\033[0m\n' "$*" >> $log; exit 0; fi
printf '\033[1;32m$\033[0m ' >> $log
type_out "$*"
cd $HOME/work
bash -c "$*" 2>&1 | tee /root/dvf/film/$role.last | short >> $log
