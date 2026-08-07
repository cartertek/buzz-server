#!/bin/sh
set -eu

wait_for_health() {
  attempts=0
  while [ "$attempts" -lt 90 ]; do
    if /usr/local/bin/buzz-server health >/dev/null 2>&1; then return 0; fi
    attempts=$((attempts + 1))
    sleep 1
  done
  return 1
}

[ "$#" -ge 1 ] && [ "$#" -le 2 ] || { echo "usage: rotate-owner.sh NEW_OWNER_SECRET_FILE [KMS_KEY_ID]" >&2; exit 64; }
secret=$1
kms_key_id=${2:-${BUZZ_KMS_KEY_ID:-}}
secretsctl=${BUZZ_SECRETSCTL:-/opt/buzz-server/current/buzz-secretsctl}
prepare=${BUZZ_PREPARE_OWNER:-/opt/buzz-server/current/share/deploy/prepare-owner-credential.sh}
envelope=/etc/buzz-server/owner-secret.envelope.json
key_file=/etc/buzz-server/owner-secret
marker=/etc/buzz-server/owner-secret.keyring
backup_dir=$(mktemp -d)
trap 'rm -rf "$backup_dir"' EXIT HUP INT TERM

test -f "$secret"
BUZZ_OWNER_RUNTIME_SECRET="$backup_dir/previous-owner" "$prepare"
previous_mode=local
if [ -f "$envelope" ]; then previous_mode=kms; fi
for path in "$envelope" "$key_file" "$marker"; do
  [ ! -e "$path" ] || cp -a "$path" "$backup_dir/$(basename "$path")"
done
rm -f "$envelope"
"$secretsctl" clear-local --key-file "$key_file" --marker "$marker"
if [ -n "$kms_key_id" ]; then
  "$secretsctl" encrypt --kms-key-id "$kms_key_id" --input "$secret" --output "$envelope"
  chown root:root "$envelope"
  chmod 0400 "$envelope"
else
  "$secretsctl" persist --input "$secret" --key-file "$key_file" --marker "$marker"
  [ ! -e "$key_file" ] || { chown root:root "$key_file"; chmod 0400 "$key_file"; }
  [ ! -e "$marker" ] || { chown root:root "$marker"; chmod 0600 "$marker"; }
fi
if ! systemctl restart buzz-server.service || ! wait_for_health; then
  rm -f "$envelope"
  "$secretsctl" clear-local --key-file "$key_file" --marker "$marker"
  if [ "$previous_mode" = kms ]; then
    cp -a "$backup_dir/$(basename "$envelope")" "$envelope"
  else
    "$secretsctl" persist --input "$backup_dir/previous-owner" --key-file "$key_file" --marker "$marker"
    [ ! -e "$key_file" ] || { chown root:root "$key_file"; chmod 0400 "$key_file"; }
    [ ! -e "$marker" ] || { chown root:root "$marker"; chmod 0600 "$marker"; }
  fi
  if systemctl restart buzz-server.service; then wait_for_health || true; fi
  echo "owner rotation failed; previous custody state restored" >&2
  exit 1
fi
echo "owner key rotated; reauthorization of existing agents must be confirmed through the relay"
