#!/bin/sh
set -eu

secretsctl=${BUZZ_SECRETSCTL:-/opt/buzz-server/current/buzz-secretsctl}
envelope=${BUZZ_OWNER_ENVELOPE:-/etc/buzz-server/owner-secret.envelope.json}
output=${BUZZ_OWNER_RUNTIME_SECRET:-/run/buzz-server/credentials/owner-secret}

install -d -o root -g root -m 0700 "$(dirname "$output")"
rm -f "$output"
"$secretsctl" decrypt --input "$envelope" --output "$output"
chown root:root "$output"
chmod 0400 "$output"
