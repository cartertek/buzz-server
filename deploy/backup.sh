#!/bin/sh
set -eu

[ "$#" -ge 1 ] && [ "$#" -le 2 ] || { echo "usage: backup.sh OUTPUT [KMS_KEY_ID]" >&2; exit 64; }
output=$1
kms_key_id=${2:-${BUZZ_KMS_KEY_ID:-}}
passphrase_file=${BUZZ_BACKUP_PASSPHRASE_FILE:-}
secretsctl=${BUZZ_SECRETSCTL:-/opt/buzz-server/current/buzz-secretsctl}
temporary=$(mktemp -d)
archive="$temporary/buzz-server-backup.tar"
root="$temporary/root"
was_active=false
cleanup() {
  rm -rf "$temporary"
  if [ "$was_active" = true ]; then systemctl start buzz-server.service >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT HUP INT TERM

if [ -z "$kms_key_id" ]; then
  [ -n "$passphrase_file" ] && [ -f "$passphrase_file" ] || { echo "backup requires KMS_KEY_ID or BUZZ_BACKUP_PASSPHRASE_FILE" >&2; exit 64; }
fi
for path in /etc/buzz-server /var/lib/buzz-server; do
  test -e "$path" || { echo "required backup path missing: $path" >&2; exit 66; }
done
mkdir -p "$root/etc" "$root/var/lib" "$root/var/log"

# Secret Service entries are not portable. Materialize the owner before
# stopping the service, export a verified NIP-49 recovery artifact, and omit
# the host-specific marker from the staged copy.
if [ -f /etc/buzz-server/owner-secret.keyring ]; then
  [ -n "$passphrase_file" ] && [ -f "$passphrase_file" ] || {
    echo "keyring-backed backup requires BUZZ_BACKUP_PASSPHRASE_FILE for NIP-49 recovery" >&2
    exit 64
  }
  staged_owner="$temporary/owner-secret"
  "$secretsctl" materialize \
    --output "$staged_owner" \
    --key-file /etc/buzz-server/owner-secret \
    --marker /etc/buzz-server/owner-secret.keyring
  "$secretsctl" export-nip49 \
    --input "$staged_owner" \
    --output "$temporary/owner-secret.ncryptsec" \
    --passphrase-file "$passphrase_file"
fi

if systemctl is-active --quiet buzz-server.service; then
  was_active=true
  systemctl stop buzz-server.service
fi
cp -a /etc/buzz-server "$root/etc/"
cp -a /var/lib/buzz-server "$root/var/lib/"
[ ! -d /var/log/buzz-server ] || cp -a /var/log/buzz-server "$root/var/log/"
if [ -f "$temporary/owner-secret.ncryptsec" ]; then
  install -m 0400 "$temporary/owner-secret.ncryptsec" \
    "$root/etc/buzz-server/owner-secret.ncryptsec"
  rm -f "$root/etc/buzz-server/owner-secret.keyring"
fi
manifest="$root/MANIFEST"
{
  printf 'format=buzz-server-backup-v2\n'
  printf 'created_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'release=%s\n' "$(readlink -f /opt/buzz-server/current 2>/dev/null || true)"
  printf 'config_sha256=%s\n' "$(sha256sum /etc/buzz-server/config.json | awk '{print $1}')"
} > "$manifest"
(cd "$root" && tar --numeric-owner --xattrs --acls -cf "$archive" etc/buzz-server var/lib/buzz-server $( [ ! -d var/log/buzz-server ] || printf '%s' var/log/buzz-server ) MANIFEST)
if [ -n "$kms_key_id" ]; then
  "$secretsctl" encrypt --kms-key-id "$kms_key_id" --input "$archive" --output "$output"
else
  "$secretsctl" encrypt-passphrase --input "$archive" --output "$output" --passphrase-file "$passphrase_file"
fi
chmod 0600 "$output"
if [ "$was_active" = true ]; then systemctl start buzz-server.service; was_active=false; /usr/local/sbin/buzz-serverctl health >/dev/null; fi
printf '%s\n' "$output"
