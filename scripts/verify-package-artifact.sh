#!/bin/sh
set -eu

for command in tar gzip sha256sum awk sed grep find stat cmp mktemp; do
  command -v "$command" >/dev/null 2>&1 || { echo "required command not found: $command" >&2; exit 69; }
done

if [ "$#" -lt 4 ] || [ "$#" -gt 5 ]; then
  echo "usage: verify-package-artifact.sh ARCHIVE CHECKSUM IDENTITY TARGET [PAYLOAD_DIR]" >&2
  exit 64
fi

archive=$1
checksum=$2
identity=$3
target=$4
payload_dir=${5:-}

case "$identity" in *[!A-Za-z0-9._-]*|'') echo "identity contains unsafe characters" >&2; exit 64 ;; esac
case "$target" in x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu) ;; *) echo "unsupported target" >&2; exit 64 ;; esac
[ -f "$archive" ] || { echo "archive not found: $archive" >&2; exit 66; }
[ -f "$checksum" ] || { echo "checksum not found: $checksum" >&2; exit 66; }

archive_dir=$(CDPATH= cd -- "$(dirname -- "$archive")" && pwd)
archive_name=$(basename "$archive")
checksum_name=$(basename "$checksum")
expected_archive="buzz-server-${target}.tar.gz"
[ "$archive_name" = "$expected_archive" ] || { echo "unexpected archive name: $archive_name" >&2; exit 65; }
[ "$checksum_name" = "$expected_archive.sha256" ] || { echo "unexpected checksum name: $checksum_name" >&2; exit 65; }
(cd "$archive_dir" && sha256sum -c "$checksum_name")

package=buzz-server
expected_manifest=$(cat <<EOF_MANIFEST
$package/
$package/buzz-server
$package/buzz-server-daemon
$package/buzz-agentctl
$package/buzz-secretsctl
$package/buzz-runtime-probe
$package/buzz-cli
$package/config/
$package/config/buzz-server.dev.example.json
$package/config/buzz-server.schema.json
$package/deploy/
$package/deploy/README.md
$package/deploy/buzz-server.service
$package/deploy/buzz-server-healthcheck.service
$package/deploy/buzz-server-healthcheck.timer
$package/deploy/backup.sh
$package/deploy/healthcheck.sh
$package/deploy/restore.sh
$package/deploy/buzz-serverctl
$package/deploy/install.sh
$package/deploy/install-package.sh
$package/deploy/install-release.sh
$package/deploy/migrate-legacy-owner.py
$package/deploy/provision-runtimes.sh
EOF_MANIFEST
)
actual_manifest=$(tar -tzf "$archive" | LC_ALL=C sort)
expected_sorted=$(printf '%s\n' "$expected_manifest" | LC_ALL=C sort)
[ "$actual_manifest" = "$expected_sorted" ] || {
  echo "archive manifest does not match the package contract" >&2
  exit 65
}

tar -tzf "$archive" | while IFS= read -r member; do
  case "$member" in "$package"|"$package"/*) ;; *) echo "unsafe archive member: $member" >&2; exit 65 ;; esac
  case "/$member/" in */../*) echo "unsafe archive traversal: $member" >&2; exit 65 ;; esac
done
if tar -tvzf "$archive" | awk 'substr($1, 1, 1) !~ /^[-d]$/ { found=1 } END { exit found ? 0 : 1 }'; then
  echo "archive must contain only regular files and directories" >&2
  exit 65
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
trap 'exit 130' INT
trap 'exit 143' HUP TERM
tar --no-same-owner --no-same-permissions -C "$work" -xzf "$archive"
root="$work/$package"

for file in buzz-server buzz-server-daemon buzz-agentctl buzz-secretsctl buzz-runtime-probe buzz-cli \
  deploy/install.sh deploy/install-package.sh deploy/install-release.sh deploy/buzz-serverctl \
  deploy/provision-runtimes.sh deploy/migrate-legacy-owner.py deploy/backup.sh deploy/restore.sh \
  deploy/healthcheck.sh; do
  [ -f "$root/$file" ] && [ -x "$root/$file" ] || { echo "expected executable missing or not executable: $file" >&2; exit 65; }
  [ "$(stat -c %a "$root/$file")" = 755 ] || { echo "unexpected executable mode for $file" >&2; exit 65; }
done
for file in config/buzz-server.dev.example.json config/buzz-server.schema.json deploy/README.md \
  deploy/buzz-server.service deploy/buzz-server-healthcheck.service deploy/buzz-server-healthcheck.timer; do
  [ -f "$root/$file" ] || { echo "expected data file missing: $file" >&2; exit 65; }
  [ "$(stat -c %a "$root/$file")" = 644 ] || { echo "unexpected data-file mode for $file" >&2; exit 65; }
done

if grep -R -n -E '@BUZZ_SERVER_(IDENTITY|TARGET)@' "$root"; then
  echo "unresolved package placeholder found" >&2
  exit 65
fi
grep -Fx "identity='${identity}'" "$root/buzz-server" >/dev/null
grep -Fx "identity='${identity}'" "$root/deploy/install.sh" >/dev/null
grep -Fx "target='${target}'" "$root/deploy/install.sh" >/dev/null

if [ -n "$payload_dir" ]; then
  payload_dir=$(CDPATH= cd -- "$payload_dir" && pwd)
  for pair in \
    "buzz-server-daemon:buzz-server-daemon" \
    "buzz-agentctl:buzz-agentctl" \
    "buzz-secretsctl:buzz-secretsctl" \
    "buzz-runtime-probe:buzz-runtime-probe" \
    "buzz-cli:buzz-cli"; do
    packaged=${pair%%:*}
    payload=${pair#*:}
    cmp "$root/$packaged" "$payload_dir/$payload"
  done
  cmp "$root/config/buzz-server.dev.example.json" "$payload_dir/config/buzz-server.dev.example.json"
  cmp "$root/config/buzz-server.schema.json" "$payload_dir/config/buzz-server.schema.json"
fi
