#!/bin/sh
set -eu

metrics=${BUZZ_METRICS_FILE:-/var/lib/buzz-server/metrics.prom}
alert=${BUZZ_ALERT_COMMAND:-}
temporary="${metrics}.tmp"
healthy=1
reason=ok
if ! systemctl is-active --quiet buzz-server.service; then healthy=0; reason=service_inactive
elif ! test -f /run/buzz-server/signer/ready; then healthy=0; reason=not_ready
elif ! test -S /run/buzz-server/lifecycle.sock; then healthy=0; reason=api_socket_missing
elif ! test -s /var/lib/buzz-server/state.sqlite3; then healthy=0; reason=database_missing
fi
install -d -o root -g buzz-server -m 0755 "$(dirname "$metrics")"
{
  echo '# HELP buzz_server_healthy Whether the daemon passed host health checks.'
  echo '# TYPE buzz_server_healthy gauge'
  echo "buzz_server_healthy $healthy"
  echo '# HELP buzz_server_healthcheck_timestamp_seconds Last health-check completion time.'
  echo '# TYPE buzz_server_healthcheck_timestamp_seconds gauge'
  echo "buzz_server_healthcheck_timestamp_seconds $(date +%s)"
} > "$temporary"
chown buzz-server:buzz-server "$temporary"
chmod 0644 "$temporary"
mv -f "$temporary" "$metrics"
if [ "$healthy" -ne 1 ]; then
  logger -p daemon.err -t buzz-server-healthcheck "health check failed: $reason"
  if [ -n "$alert" ]; then BUZZ_ALERT_REASON=$reason sh -c "$alert"; fi
  exit 1
fi
