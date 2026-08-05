#!/bin/sh
set -eu

[ "$#" -eq 2 ] || { echo "usage: rotate-owner.sh KMS_KEY_ID NEW_OWNER_SECRET_FILE" >&2; exit 64; }
kms_key_id=$1
secret=$2
secretsctl=${BUZZ_SECRETSCTL:-/opt/buzz-server/current/buzz-secretsctl}
envelope=/etc/buzz-server/owner-secret.envelope.json
next="$envelope.next"
backup="$envelope.previous"

test -f "$secret"
"$secretsctl" encrypt --kms-key-id "$kms_key_id" --input "$secret" --output "$next"
chown root:root "$next"
chmod 0400 "$next"
cp -a "$envelope" "$backup"
mv -f "$next" "$envelope"
if ! systemctl restart buzz-server.service || ! /usr/local/sbin/buzz-serverctl health >/dev/null; then
  mv -f "$backup" "$envelope"
  systemctl restart buzz-server.service || true
  echo "owner rotation failed; previous envelope restored" >&2
  exit 1
fi
rm -f "$backup"
echo "owner key rotated; reauthorization of existing agents must be confirmed through the relay"
