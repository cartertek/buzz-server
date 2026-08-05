#!/bin/sh
set -eu

[ "$#" -eq 1 ] || { echo "usage: restore.sh BACKUP.envelope.json" >&2; exit 64; }
backup=$1
secretsctl=${BUZZ_SECRETSCTL:-/opt/buzz-server/current/buzz-secretsctl}
temporary=$(mktemp -d)
archive="$temporary/backup.tar"
staging="$temporary/root"
cleanup() { rm -rf "$temporary"; }
trap cleanup EXIT HUP INT TERM

"$secretsctl" decrypt --input "$backup" --output "$archive"
tar -tf "$archive" | while IFS= read -r member; do
  case "$member" in etc/buzz-server/*|var/lib/buzz-server/*|var/log/buzz-server/*|MANIFEST) ;;
    *) echo "unsafe backup member: $member" >&2; exit 65;;
  esac
  case "/$member/" in */../*) echo "backup contains path traversal" >&2; exit 65;; esac
done
if tar -tvf "$archive" | awk 'substr($1, 1, 1) !~ /^[-d]$/ { found=1 } END { exit found ? 0 : 1 }'; then
  echo "backup must contain only regular files and directories" >&2
  exit 65
fi
mkdir "$staging"
tar --numeric-owner --xattrs --acls -C "$staging" -xf "$archive"
test -f "$staging/etc/buzz-server/config.json"
test -f "$staging/var/lib/buzz-server/state.sqlite3"
test -f "$staging/MANIFEST"
grep -qx 'format=buzz-server-backup-v1' "$staging/MANIFEST"
expected=$(sed -n 's/^config_sha256=//p' "$staging/MANIFEST")
actual=$(sha256sum "$staging/etc/buzz-server/config.json" | awk '{print $1}')
[ -n "$expected" ] && [ "$expected" = "$actual" ] || { echo "backup config digest mismatch" >&2; exit 65; }

systemctl stop buzz-server.service
snapshot="/var/lib/buzz-server.restore-$(date -u +%Y%m%dT%H%M%SZ)"
if [ -e /var/lib/buzz-server ]; then mv /var/lib/buzz-server "$snapshot"; fi
rm -rf /etc/buzz-server.restore-staging
mv "$staging/var/lib/buzz-server" /var/lib/buzz-server
cp -a "$staging/etc/buzz-server/." /etc/buzz-server/
if [ -d "$staging/var/log/buzz-server" ]; then
  install -d -o buzz-server -g buzz-server -m 0700 /var/log/buzz-server
  cp -a "$staging/var/log/buzz-server/." /var/log/buzz-server/
fi
systemctl start buzz-server.service
if ! /usr/local/sbin/buzz-serverctl health >/dev/null; then
  systemctl stop buzz-server.service || true
  rm -rf /var/lib/buzz-server
  if [ -e "$snapshot" ]; then mv "$snapshot" /var/lib/buzz-server; fi
  systemctl start buzz-server.service || true
  echo "restored backup failed health checks; previous state restored" >&2
  exit 1
fi
rm -rf "$snapshot"
echo restored
