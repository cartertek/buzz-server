#!/bin/sh
set -eu

secretsctl=${BUZZ_SECRETSCTL:-/opt/buzz-server/current/buzz-secretsctl}
envelope=${BUZZ_OWNER_ENVELOPE:-/etc/buzz-server/owner-secret.envelope.json}
key_file=${BUZZ_OWNER_KEY_FILE:-/etc/buzz-server/owner-secret}
marker=${BUZZ_OWNER_KEYRING_MARKER:-/etc/buzz-server/owner-secret.keyring}
output=${BUZZ_OWNER_RUNTIME_SECRET:-/run/buzz-server/credentials/owner-secret}

install -d -o root -g root -m 0700 "$(dirname "$output")"
rm -f "$output"
if [ -f "$envelope" ]; then
  "$secretsctl" decrypt --input "$envelope" --output "$output"
else
  "$secretsctl" materialize --output "$output" --key-file "$key_file" --marker "$marker"
fi
chown root:root "$output"
chmod 0400 "$output"
