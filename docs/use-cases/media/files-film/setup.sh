#!/bin/bash
# setup.sh ROLE: a clean home for ROLE on this desktop, and the files A sends.
# Stop a helper left from an earlier take, then start clean.
[ -x /root/dvf/$1/.local/bin/diavlos ] && HOME=/root/dvf/$1 /root/dvf/$1/.local/bin/diavlos stop >/dev/null 2>&1
role=$1
rm -rf /root/dvf/$role; mkdir -p /root/dvf/$role/work /root/dvf/film
name=$([ $role = a ] && echo desk-a || echo desk-b)
cat > /root/dvf/film/$role.env <<E
export HOME=/root/dvf/$role USER=$name PATH=/root/dvf/$role/.local/bin:/usr/bin:/bin
unset DIAVLOS_HOME DIAVLOS_AS
E
if [ $role = a ]; then
  printf 'port = 8080\nworkers = 2\nlog = info\n' > /root/dvf/a/work/app.conf
  printf '#!/bin/sh\necho "restarting api"\nsystemctl restart api\n' > /root/dvf/a/work/restart.sh
  chmod +x /root/dvf/a/work/restart.sh
fi
