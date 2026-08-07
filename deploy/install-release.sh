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
cargo_version=${version#v}
[ -n "$cargo_version" ] || { echo "tag has no Cargo version" >&2; exit 64; }

asset="buzz-server-${target}.tar.gz"
base="https://github.com/${repository}/releases/download/${version}"
temporary=$(mktemp -d)
release_staging=
cleanup() {
  rm -rf "$temporary"
  if [ -n "$release_staging" ]; then
    rm -rf "$release_staging"
  fi
}
trap cleanup EXIT HUP INT TERM
package="buzz-server"
expected_manifest=$(cat <<EOF
$package/
$package/buzz-server
$package/buzz-agentctl
$package/buzz-secretsctl
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
$package/deploy/disaster-recovery-exercise.sh
$package/deploy/prepare-owner-credential.sh
$package/deploy/restore.sh
$package/deploy/rotate-owner.sh
$package/deploy/buzz-serverctl
$package/deploy/install.sh
$package/deploy/install-release.sh
$package/deploy/provision-runtimes.sh
EOF
)
if [ -n "${BUZZ_RELEASE_SOURCE_DIR:-}" ]; then
  source_directory=$(cd "$BUZZ_RELEASE_SOURCE_DIR" && pwd)
  [ "$(basename "$source_directory")" = "$package" ] || {
    echo "local package directory must be named $package" >&2
    exit 65
  }
else
  curl -fsSL -o "$temporary/$asset" "$base/$asset"
  curl -fsSL -o "$temporary/$asset.sha256" "$base/$asset.sha256"
  (cd "$temporary" && sha256sum -c "$asset.sha256")
  actual_manifest=$(tar -tzf "$temporary/$asset" | LC_ALL=C sort)
  [ "$actual_manifest" = "$(printf '%s\n' "$expected_manifest" | LC_ALL=C sort)" ] || {
    echo "archive manifest does not match the release contract" >&2
    exit 65
  }
  tar -tzf "$temporary/$asset" | while IFS= read -r member; do
    case "$member" in
      "$package"|"$package"/*) ;;
      *) echo "unsafe archive member: $member" >&2; exit 65;;
    esac
    case "/$member/" in */../*) echo "unsafe archive traversal" >&2; exit 65;; esac
  done
  if tar -tvzf "$temporary/$asset" | awk 'substr($1, 1, 1) !~ /^[-d]$/ { found=1 } END { exit found ? 0 : 1 }'; then
    echo "archive must contain only regular files and directories" >&2
    exit 65
  fi
  tar --no-same-owner --no-same-permissions -C "$temporary" -xzf "$temporary/$asset"
  source_directory="$temporary/$package"
fi
test -x "$source_directory/buzz-server"
test -x "$source_directory/buzz-agentctl"
test -x "$source_directory/buzz-secretsctl"
release="/opt/buzz-server/releases/$version-$target"
previous=$(readlink -f /opt/buzz-server/current 2>/dev/null || true)
if ! getent group buzz-server >/dev/null 2>&1; then
  groupadd --system buzz-server
fi
if ! id buzz-server >/dev/null 2>&1; then
  useradd --system --gid buzz-server --home-dir /var/lib/buzz-server --shell /usr/sbin/nologin buzz-server
fi
if ! getent group buzz-agent >/dev/null 2>&1; then
  groupadd --system buzz-agent
fi
if ! id buzz-agent >/dev/null 2>&1; then
  useradd --system --gid buzz-agent --home-dir /var/lib/buzz-server/runtime --shell /usr/sbin/nologin buzz-agent
fi
install -d -o buzz-agent -g buzz-agent -m 0700 \
  /var/lib/buzz-server/workspaces \
  /var/lib/buzz-server/runtime \
  /var/lib/buzz-server/runtime/agent \
  /var/lib/buzz-server/runtime/agent/codex-home \
  /var/lib/buzz-server/runtime/agent/tmp
install -d -o root -g root -m 0755 /opt/buzz-server /opt/buzz-server/releases
[ ! -e "$release" ] && [ ! -L "$release" ] || {
  echo "release $version-$target is already installed; immutable releases are never overwritten" >&2
  exit 73
}
install -d -o root -g buzz-server -m 0750 /etc/buzz-server
install -d -o buzz-server -g buzz-server -m 0755 /var/lib/buzz-server
install -d -o buzz-server -g buzz-server -m 0700 /var/log/buzz-server
install -d -o root -g root -m 0755 /usr/libexec/buzz-server
release_staging=$(mktemp -d "/opt/buzz-server/releases/.${version}-${target}.staging.XXXXXX")
install -o root -g root -m 0555 "$source_directory/buzz-server" "$release_staging/buzz-server"
install -o root -g root -m 0555 "$source_directory/buzz-agentctl" "$release_staging/buzz-agentctl"
install -o root -g root -m 0555 "$source_directory/buzz-secretsctl" "$release_staging/buzz-secretsctl"
install -d -o root -g root -m 0555 "$release_staging/share"
cp -R "$source_directory/config" "$source_directory/deploy" "$release_staging/share/"
chown -R root:root "$release_staging"
find "$release_staging/share" -type d -exec chmod 0555 {} +
find "$release_staging/share" -type f -exec chmod 0444 {} +
chmod 0555 \
  "$release_staging/share/deploy/buzz-serverctl" \
  "$release_staging/share/deploy/install.sh" \
  "$release_staging/share/deploy/install-release.sh" \
  "$release_staging/share/deploy/provision-runtimes.sh" \
  "$release_staging/share/deploy/prepare-owner-credential.sh" \
  "$release_staging/share/deploy/backup.sh" \
  "$release_staging/share/deploy/restore.sh" \
  "$release_staging/share/deploy/rotate-owner.sh" \
  "$release_staging/share/deploy/healthcheck.sh" \
  "$release_staging/share/deploy/disaster-recovery-exercise.sh"
chmod 0555 "$release_staging"
if [ ! -e /etc/buzz-server/config.json ]; then
  config_source=${BUZZ_CONFIG_FILE:-$source_directory/config/buzz-server.dev.example.json}
  test -f "$config_source"
  install -o root -g buzz-server -m 0640 "$config_source" /etc/buzz-server/config.json
fi
if grep -q '"owner_secret_file": "/run/credentials/buzz-server.service/owner-secret"' /etc/buzz-server/config.json; then
  sed -i 's#"owner_secret_file": "/run/credentials/buzz-server.service/owner-secret"#"owner_secret_file": "/run/buzz-server/credentials/owner-secret"#' /etc/buzz-server/config.json
fi
if [ ! -e /etc/buzz-server/secrets.env ]; then
  secrets_source=${BUZZ_SECRETS_FILE:-/dev/null}
  test -f "$secrets_source"
  install -o root -g buzz-server -m 0640 "$secrets_source" /etc/buzz-server/secrets.env
fi
owner_envelope=/etc/buzz-server/owner-secret.envelope.json
owner_key_file=/etc/buzz-server/owner-secret
owner_marker=/etc/buzz-server/owner-secret.keyring
if [ ! -e "$owner_envelope" ] && [ ! -e "$owner_key_file" ] && [ ! -e "$owner_marker" ]; then
  owner_source=${BUZZ_OWNER_SECRET_FILE:-}
  if [ -n "${BUZZ_OWNER_ENVELOPE_FILE:-}" ] && [ -f "$BUZZ_OWNER_ENVELOPE_FILE" ]; then
    install -o root -g root -m 0400 "$BUZZ_OWNER_ENVELOPE_FILE" "$owner_envelope"
  else
    [ -n "$owner_source" ] && [ -f "$owner_source" ] || {
      echo "first install requires BUZZ_OWNER_ENVELOPE_FILE or BUZZ_OWNER_SECRET_FILE" >&2
      exit 66
    }
    if [ -n "${BUZZ_KMS_KEY_ID:-}" ]; then
      "$release_staging/buzz-secretsctl" encrypt --kms-key-id "$BUZZ_KMS_KEY_ID" --input "$owner_source" --output "$owner_envelope"
      chown root:root "$owner_envelope"
      chmod 0400 "$owner_envelope"
    else
      "$release_staging/buzz-secretsctl" persist --input "$owner_source" --key-file "$owner_key_file" --marker "$owner_marker"
      [ ! -e "$owner_key_file" ] || { chown root:root "$owner_key_file"; chmod 0400 "$owner_key_file"; }
      [ ! -e "$owner_marker" ] || { chown root:root "$owner_marker"; chmod 0600 "$owner_marker"; }
    fi
  fi
fi
runtime_assets_valid() {
  harness_dir=/opt/buzz-server/runtimes/sprig-0.1.0
  runtime_dir=/opt/buzz-server/runtimes/codex-acp-1.1.7
  [ -x "$harness_dir/bin/buzz-acp" ] && [ -f "$harness_dir/.package.sha256" ] &&
    [ -x "$runtime_dir/bin/codex-acp" ] && [ -f "$runtime_dir/.package.sha256" ] &&
    (cd "$harness_dir" && sha256sum -c .package.sha256 >/dev/null) &&
    (cd "$runtime_dir" && sha256sum -c .package.sha256 >/dev/null) &&
    [ "$(stat -c '%U:%G' "$harness_dir")" = root:buzz-agent ] &&
    [ "$(stat -c '%U:%G' "$runtime_dir")" = root:buzz-agent ] &&
    ! find "$harness_dir" \( ! -user root -o -perm /022 \) -print -quit | grep -q . &&
    ! find "$runtime_dir" \( ! -user root -o -perm /022 \) -print -quit | grep -q .
}
if ! runtime_assets_valid; then
  if [ -n "${BUZZ_HARNESS_URL:-}" ] && [ -n "${BUZZ_HARNESS_SHA256:-}" ] && [ -n "${BUZZ_RUNTIME_URL:-}" ] && [ -n "${BUZZ_RUNTIME_SHA256:-}" ]; then
    "$release_staging/share/deploy/provision-runtimes.sh" "$BUZZ_HARNESS_URL" "$BUZZ_HARNESS_SHA256" "$BUZZ_RUNTIME_URL" "$BUZZ_RUNTIME_SHA256"
  else
    echo "pinned runtime assets are absent; provide BUZZ_HARNESS_URL/SHA256 and BUZZ_RUNTIME_URL/SHA256" >&2
    exit 66
  fi
fi
runtime_assets_valid || { echo "pinned runtime asset validation failed" >&2; exit 66; }
/usr/bin/timeout 30s /usr/sbin/runuser --user buzz-agent -- /usr/bin/env -i \
  HOME=/var/lib/buzz-server/runtime/agent \
  CODEX_HOME=/var/lib/buzz-server/runtime/agent/codex-home \
  TMPDIR=/var/lib/buzz-server/runtime/agent/tmp \
  PATH=/usr/local/bin:/usr/bin:/bin \
  /opt/buzz-server/runtimes/sprig-0.1.0/bin/buzz-acp models --json \
  --agent-command /opt/buzz-server/runtimes/codex-acp-1.1.7/bin/codex-acp \
  --agent-args acp >/dev/null || {
    echo "pinned runtime packages failed the isolated buzz-agent preflight" >&2
    exit 66
  }
unit_backup="$temporary/buzz-server.service.previous"
unit_existed=false
if [ -e /etc/systemd/system/buzz-server.service ] || [ -L /etc/systemd/system/buzz-server.service ]; then
  cp -L /etc/systemd/system/buzz-server.service "$unit_backup"
  unit_existed=true
fi
mv -T "$release_staging" "$release"
release_staging=
ln -sfn "$release" /opt/buzz-server/current.next
mv -Tf /opt/buzz-server/current.next /opt/buzz-server/current
install -o root -g root -m 0444 "$release/share/deploy/buzz-server.service" /etc/systemd/system/buzz-server.service
ln -sfn /opt/buzz-server/current/share/deploy/install-release.sh /usr/libexec/buzz-server/install-release.sh
ln -sfn /opt/buzz-server/current/share/deploy/buzz-serverctl /usr/local/sbin/buzz-serverctl
ln -sfn /opt/buzz-server/current/buzz-agentctl /usr/local/bin/buzz-agentctl
ln -sfn /opt/buzz-server/current/buzz-secretsctl /usr/local/sbin/buzz-secretsctl
install -o root -g root -m 0444 "$release/share/deploy/buzz-server-healthcheck.service" /etc/systemd/system/buzz-server-healthcheck.service
install -o root -g root -m 0444 "$release/share/deploy/buzz-server-healthcheck.timer" /etc/systemd/system/buzz-server-healthcheck.timer
systemctl daemon-reload
systemctl enable buzz-server.service buzz-server-healthcheck.timer
wait_for_health() {
  healthy=false
  attempts=0
  while [ "$attempts" -lt 90 ]; do
    if /usr/local/sbin/buzz-serverctl health >/dev/null 2>&1; then
      return 0
    fi
    attempts=$((attempts + 1))
    sleep 1
  done
  return 1
}
systemctl restart buzz-server-healthcheck.timer
if ! systemctl restart buzz-server.service || ! wait_for_health; then
  if [ -n "$previous" ] && [ -x "$previous/buzz-server" ]; then
    ln -sfn "$previous" /opt/buzz-server/current.next
    mv -Tf /opt/buzz-server/current.next /opt/buzz-server/current
    if [ "$unit_existed" = true ]; then
      install -o root -g root -m 0444 "$unit_backup" /etc/systemd/system/buzz-server.service
    else
      rm -f /etc/systemd/system/buzz-server.service
    fi
    systemctl daemon-reload
    systemctl restart buzz-server.service && wait_for_health || {
      echo "deployment and automatic rollback both failed health checks" >&2
      exit 1
    }
  else
    rm -f /opt/buzz-server/current
    systemctl stop buzz-server.service >/dev/null 2>&1 || true
  fi
  echo "deployment failed; previous release restored when available" >&2
  exit 1
fi
