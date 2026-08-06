#!/bin/sh
set -eu

[ "$#" -eq 1 ] || { echo "usage: restore.sh BACKUP" >&2; exit 64; }
backup=$1
passphrase_file=${BUZZ_BACKUP_PASSPHRASE_FILE:-}
secretsctl=${BUZZ_SECRETSCTL:-/opt/buzz-server/current/buzz-secretsctl}
temporary=$(mktemp -d)
archive="$temporary/backup.tar"
staging="$temporary/root"
cleanup() { rm -rf "$temporary"; }
trap cleanup EXIT HUP INT TERM

if grep -q '"kms_key_id"' "$backup" 2>/dev/null; then
  "$secretsctl" decrypt --input "$backup" --output "$archive"
else
  [ -n "$passphrase_file" ] && [ -f "$passphrase_file" ] || { echo "passphrase backup requires BUZZ_BACKUP_PASSPHRASE_FILE" >&2; exit 64; }
  "$secretsctl" decrypt-passphrase --input "$backup" --output "$archive" --passphrase-file "$passphrase_file"
fi
tar -tf "$archive" | while IFS= read -r member; do
  case "$member" in etc/buzz-server/*|var/lib/buzz-server/*|var/log/buzz-server/*|MANIFEST) ;; *) echo "unsafe backup member: $member" >&2; exit 65;; esac
  case "/$member/" in */../*) echo "backup contains path traversal" >&2; exit 65;; esac
done
if tar -tvf "$archive" | awk 'substr($1, 1, 1) !~ /^[-d]$/ { found=1 } END { exit found ? 0 : 1 }'; then echo "backup must contain only regular files and directories" >&2; exit 65; fi
mkdir "$staging"
tar --numeric-owner --xattrs --acls -C "$staging" -xf "$archive"
test -f "$staging/etc/buzz-server/config.json"
test -f "$staging/var/lib/buzz-server/state.sqlite3"
test -f "$staging/MANIFEST"
grep -Eqx 'format=buzz-server-backup-v[12]' "$staging/MANIFEST"
expected=$(sed -n 's/^config_sha256=//p' "$staging/MANIFEST")
actual=$(sha256sum "$staging/etc/buzz-server/config.json" | awk '{print $1}')
[ -n "$expected" ] && [ "$expected" = "$actual" ] || { echo "backup config digest mismatch" >&2; exit 65; }

# Convert a portable Desktop-compatible owner backup into normal local custody
# before replacing live configuration. Never leave ncryptsec in /etc.
if [ -f "$staging/etc/buzz-server/owner-secret.ncryptsec" ]; then
  [ -n "$passphrase_file" ] && [ -f "$passphrase_file" ] || { echo "owner recovery requires BUZZ_BACKUP_PASSPHRASE_FILE" >&2; exit 64; }
  recovered="$temporary/recovered-owner"
  "$secretsctl" import-nip49 --input "$staging/etc/buzz-server/owner-secret.ncryptsec" --output "$recovered" --passphrase-file "$passphrase_file"
  rm -f "$staging/etc/buzz-server/owner-secret.ncryptsec" "$staging/etc/buzz-server/owner-secret.envelope.json" "$staging/etc/buzz-server/owner-secret" "$staging/etc/buzz-server/owner-secret.keyring"
  "$secretsctl" persist --input "$recovered" --key-file "$staging/etc/buzz-server/owner-secret" --marker "$staging/etc/buzz-server/owner-secret.keyring"
fi
systemctl stop buzz-server.service
snapshot="/var/lib/buzz-server.restore-$(date -u +%Y%m%dT%H%M%SZ)"
etc_snapshot="$temporary/etc-buzz-server.previous"
cp -a /etc/buzz-server "$etc_snapshot"
if [ -e /var/lib/buzz-server ]; then mv /var/lib/buzz-server "$snapshot"; fi
mv "$staging/var/lib/buzz-server" /var/lib/buzz-server
rm -rf /etc/buzz-server
mv "$staging/etc/buzz-server" /etc/buzz-server
if [ -d "$staging/var/log/buzz-server" ]; then install -d -o buzz-server -g buzz-server -m 0700 /var/log/buzz-server; cp -a "$staging/var/log/buzz-server/." /var/log/buzz-server/; fi
systemctl start buzz-server.service
if ! /usr/local/sbin/buzz-serverctl health >/dev/null; then
  systemctl stop buzz-server.service || true
  rm -rf /var/lib/buzz-server /etc/buzz-server
  [ ! -e "$snapshot" ] || mv "$snapshot" /var/lib/buzz-server
  cp -a "$etc_snapshot" /etc/buzz-server
  systemctl start buzz-server.service || true
  echo "restored backup failed health checks; previous state restored" >&2
  exit 1
fi
rm -rf "$snapshot"
echo restored
