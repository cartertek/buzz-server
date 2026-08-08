#!/bin/sh
set -eu

secretsctl=${BUZZ_SECRETSCTL:-/opt/buzz-server/current/buzz-secretsctl}
envelope=${BUZZ_OWNER_ENVELOPE:-/etc/buzz-server/owner-secret.envelope.json}
key_file=${BUZZ_OWNER_KEY_FILE:-/etc/buzz-server/owner-secret}
marker=${BUZZ_OWNER_KEYRING_MARKER:-/etc/buzz-server/owner-secret.keyring}
output=${BUZZ_OWNER_RUNTIME_SECRET:-/run/buzz-server/credentials/owner-secret}

install -d -o root -g root -m 0700 "$(dirname "$output")"
rm -f "$output"
if [ ! -f "$envelope" ] && [ ! -f "$key_file" ] && [ ! -f "$marker" ]; then
  exit 0
fi
if [ -f "$envelope" ]; then
  timeout 60 "$secretsctl" decrypt --input "$envelope" --output "$output" || { echo "owner credential decryption failed or timed out" >&2; exit 1; }
else
  timeout 30 "$secretsctl" materialize --output "$output" --key-file "$key_file" --marker "$marker" || { echo "owner credential materialization failed or timed out" >&2; exit 1; }
fi
chown root:root "$output"
chmod 0400 "$output"
