#!/bin/sh
# Configure the verified HL202 profile, then own live link readiness.
set -e
id="${1:-1}"
/usr/sbin/fttr-omci-service "$id" 0 --wait-o5 --transparent-defaults --restore-initial-alloc
exec /usr/sbin/fttr-omci-probe "$id" 60 --stock-omcc --1905 --watch
