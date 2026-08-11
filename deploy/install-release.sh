#!/bin/sh
set -eu

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  echo "usage: install-release.sh VERSION TARGET [OWNER/REPOSITORY]" >&2
  exit 64
fi
version=$1
target=$2
repository=${3:-cartertek/buzz-server}
case "$version" in v[0-9]* ) ;; *) echo "version must be an immutable v* tag" >&2; exit 64;; esac
case "$version" in *[!A-Za-z0-9._-]* ) echo "version contains unsafe characters" >&2; exit 64;; esac
case "$target" in x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu) ;; *) echo "unsupported target" >&2; exit 64;; esac
case "$repository" in *[!A-Za-z0-9._/-]*|*/*/*|'') echo "invalid repository" >&2; exit 64;; esac
for command in curl tar sha256sum awk mktemp; do command -v "$command" >/dev/null 2>&1 || { echo "required command not found: $command" >&2; exit 69; }; done

asset="buzz-server-${target}.tar.gz"
base="https://github.com/${repository}/releases/download/${version}"
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT
trap 'exit 130' INT
trap 'exit 143' HUP TERM

echo "==> Downloading Buzz Server $version for $target" >&2
curl --fail --location --connect-timeout 10 --max-time 120 -o "$temporary/$asset" "$base/$asset"
curl --fail --location --connect-timeout 10 --max-time 30 -o "$temporary/$asset.sha256" "$base/$asset.sha256"
(cd "$temporary" && sha256sum -c "$asset.sha256")

package=buzz-server
expected_manifest=$(cat <<EOF
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
$package/deploy/prepare-community-identities.sh
$package/deploy/restore.sh
$package/deploy/buzz-serverctl
$package/deploy/install.sh
$package/deploy/install-package.sh
$package/deploy/migrate-legacy-owner.py
$package/deploy/install-release.sh
$package/deploy/provision-runtimes.sh
EOF
)
actual_manifest=$(tar -tzf "$temporary/$asset" | LC_ALL=C sort)
[ "$actual_manifest" = "$(printf '%s\n' "$expected_manifest" | LC_ALL=C sort)" ] || { echo "archive manifest does not match the release contract" >&2; exit 65; }
tar -tzf "$temporary/$asset" | while IFS= read -r member; do
  case "$member" in "$package"|"$package"/*) ;; *) echo "unsafe archive member: $member" >&2; exit 65;; esac
  case "/$member/" in */../*) echo "unsafe archive traversal" >&2; exit 65;; esac
done
if tar -tvzf "$temporary/$asset" | awk 'substr($1, 1, 1) !~ /^[-d]$/ { found=1 } END { exit found ? 0 : 1 }'; then
  echo "archive must contain only regular files and directories" >&2
  exit 65
fi
tar --no-same-owner --no-same-permissions -C "$temporary" -xzf "$temporary/$asset"
"$temporary/$package/deploy/install-package.sh" "$version" "$target" "$temporary/$package"
