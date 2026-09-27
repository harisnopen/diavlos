#!/bin/bash
# view-log.sh ROLE: what a film window shows, ROLE's log as it grows.
tail -n +1 -F /root/dvf/film/$1.log 2>/dev/null
