#!/bin/bash
# setup.sh ROLE: a clean home for ROLE (a or b) on this desktop, and the
# files A sends. ROLE2 is a second window on the same home.
# Stop what an earlier take left running (a live window still sending as
# its agent would reach the new room), then start clean.
pkill -f '^/bin/bash /root/dvf/film/(runaway|outbox-loop|wake-view|view-log)\.sh'
[ -x /root/dvf/$1/.local/bin/diavlos ] && HOME=/root/dvf/$1 /root/dvf/$1/.local/bin/diavlos stop >/dev/null 2>&1
role=$1
rm -rf /root/dvf/$role; mkdir -p /root/dvf/$role/work /root/dvf/film
: > /root/dvf/film/$role.log; : > /root/dvf/film/${role}2.log
name=$([ $role = a ] && echo desk-a || echo desk-b)
cat > /root/dvf/film/$role.env <<E
export HOME=/root/dvf/$role USER=$name PATH=/root/dvf/$role/.local/bin:/usr/bin:/bin
unset DIAVLOS_HOME DIAVLOS_AS
E
# A second window on the same desktop, same home.
cp /root/dvf/film/$role.env /root/dvf/film/${role}2.env
if [ $role = a ]; then
  printf 'port = 8080\nworkers = 2\nlog = info\n' > /root/dvf/a/work/app.conf
  printf '#!/bin/sh\necho "restarting api"\nsystemctl restart api\n' > /root/dvf/a/work/restart.sh
  chmod +x /root/dvf/a/work/restart.sh
fi
