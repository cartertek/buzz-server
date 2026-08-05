#!/bin/sh
set -eu

[ "$#" -eq 2 ] || { echo "usage: backup.sh KMS_KEY_ID OUTPUT.envelope.json" >&2; exit 64; }
kms_key_id=$1
output=$2
secretsctl=${BUZZ_SECRETSCTL:-/opt/buzz-server/current/buzz-secretsctl}
temporary=$(mktemp -d)
archive="$temporary/buzz-server-backup.tar"
was_active=false
cleanup() {
  rm -rf "$temporary"
  if [ "$was_active" = true ]; then systemctl start buzz-server.service >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT HUP INT TERM

if systemctl is-active --quiet buzz-server.service; then
  was_active=true
  systemctl stop buzz-server.service
fi

for path in /etc/buzz-server /var/lib/buzz-server; do
  test -e "$path" || { echo "required backup path missing: $path" >&2; exit 66; }
done
manifest="$temporary/MANIFEST"
{
  printf 'format=buzz-server-backup-v1\n'
  printf 'created_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'release=%s\n' "$(readlink -f /opt/buzz-server/current 2>/dev/null || true)"
  printf 'config_sha256=%s\n' "$(sha256sum /etc/buzz-server/config.json | awk '{print $1}')"
} > "$manifest"

tar --numeric-owner --xattrs --acls -cf "$archive" \
  -C / etc/buzz-server var/lib/buzz-server var/log/buzz-server \
  -C "$temporary" MANIFEST
"$secretsctl" encrypt --kms-key-id "$kms_key_id" --input "$archive" --output "$output"
chmod 0600 "$output"
if [ "$was_active" = true ]; then
  systemctl start buzz-server.service
  was_active=false
  /usr/local/sbin/buzz-serverctl health >/dev/null
fi
printf '%s\n' "$output"
