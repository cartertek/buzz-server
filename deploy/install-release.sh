#!/bin/sh
set -eu

log() { printf '%s\n' "==> $*" >&2; }
fail() { printf 'error: %s\n' "$*" >&2; exit 1; }
run_bounded() {
  seconds=$1
  description=$2
  shift 2
  log "$description"
  timeout "$seconds" "$@" || {
    status=$?
    if [ "$status" -eq 124 ]; then
      fail "$description timed out after ${seconds}s"
    fi
    fail "$description failed (exit $status)"
  }
}
service_diagnostics() {
  printf '%s\n' '--- buzz-server service status ---' >&2
  timeout 10 systemctl status --no-pager --lines=20 buzz-server.service >&2 2>&1 || true
  tasks=$(timeout 5 systemctl show buzz-server.service -p TasksCurrent --value 2>/dev/null || true)
  tasks_max=$(timeout 5 systemctl show buzz-server.service -p TasksMax --value 2>/dev/null || true)
  printf 'buzz-server tasks: %s/%s\n' "${tasks:-unknown}" "${tasks_max:-unknown}" >&2
  printf '%s\n' '--- recent buzz-server logs ---' >&2
  timeout 10 journalctl -u buzz-server.service -n 30 --no-pager >&2 2>&1 || true
}
service_process_count() {
  control_group=$(timeout 5 systemctl show buzz-server.service -p ControlGroup --value 2>/dev/null || true)
  [ -n "$control_group" ] || { printf '0\n'; return; }
  procs="/sys/fs/cgroup${control_group}/cgroup.procs"
  [ -r "$procs" ] || { printf '0\n'; return; }
  wc -l < "$procs" | tr -d ' '
}
drain_service() {
  label=$1
  log "$label"
  timeout 20 systemctl stop buzz-server.service >/dev/null 2>&1 || true
  timeout 5 systemctl kill --kill-whom=all --signal=SIGTERM buzz-server.service >/dev/null 2>&1 || true
  elapsed=0
  while [ "$elapsed" -lt 5 ]; do
    count=$(service_process_count)
    [ "$count" -eq 0 ] && return 0
    sleep 1
    elapsed=$((elapsed + 1))
  done
  timeout 5 systemctl kill --kill-whom=all --signal=SIGKILL buzz-server.service >/dev/null 2>&1 || true
  elapsed=0
  while [ "$elapsed" -lt 5 ]; do
    count=$(service_process_count)
    [ "$count" -eq 0 ] && return 0
    sleep 1
    elapsed=$((elapsed + 1))
  done
  service_diagnostics
  fail "$label could not drain the existing service process tree"
}
wait_for_health() {
  controller=$1
  label=$2
  limit=${3:-30}
  elapsed=0
  while [ "$elapsed" -lt "$limit" ]; do
    if timeout 5 "$controller" health >/dev/null 2>&1; then
      log "$label is healthy"
      return 0
    fi
    if timeout 5 systemctl is-failed --quiet buzz-server.service; then
      printf 'error: %s entered failed state\n' "$label" >&2
      return 1
    fi
    elapsed=$((elapsed + 1))
    if [ "$elapsed" -eq 1 ] || [ $((elapsed % 5)) -eq 0 ]; then
      active=$(timeout 5 systemctl show buzz-server.service -p ActiveState --value 2>/dev/null || true)
      sub=$(timeout 5 systemctl show buzz-server.service -p SubState --value 2>/dev/null || true)
      printf 'Waiting for %s health (%ss/%ss; %s/%s)...\n' "$label" "$elapsed" "$limit" "${active:-unknown}" "${sub:-unknown}" >&2
    fi
    sleep 1
  done
  printf 'error: %s did not become healthy within %ss\n' "$label" "$limit" >&2
  return 1
}

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
for command in timeout systemctl journalctl tar sha256sum awk sed find install getent groupadd useradd runuser stat; do
  command -v "$command" >/dev/null 2>&1 || { echo "required command not found: $command" >&2; exit 69; }
done
case "$(uname -m)" in
  x86_64|amd64) host_target=x86_64-unknown-linux-gnu ;;
  aarch64|arm64) host_target=aarch64-unknown-linux-gnu ;;
  *) echo "unsupported host architecture" >&2; exit 64 ;;
esac
[ "$target" = "$host_target" ] || { echo "package target $target does not match host $host_target" >&2; exit 65; }
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
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' HUP TERM
package="buzz-server"
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
  command -v curl >/dev/null 2>&1 || { echo "required command not found: curl" >&2; exit 69; }
  log "Downloading Buzz Server $version for $target"
  curl --fail --location --connect-timeout 10 --max-time 120 -o "$temporary/$asset" "$base/$asset" || fail "release download failed"
  curl --fail --location --connect-timeout 10 --max-time 30 -o "$temporary/$asset.sha256" "$base/$asset.sha256" || fail "checksum download failed"
  log "Verifying release archive"
  (cd "$temporary" && sha256sum -c "$asset.sha256") || fail "release checksum verification failed"
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
log "Validating package contents"
test -x "$source_directory/buzz-server" || fail "package is missing buzz-server"
test -x "$source_directory/buzz-server-daemon" || fail "package is missing buzz-server-daemon"
test -x "$source_directory/buzz-agentctl" || fail "package is missing internal agent client"
test -x "$source_directory/buzz-secretsctl" || fail "package is missing internal secrets client"
test -x "$source_directory/buzz-runtime-probe" || fail "package is missing internal runtime probe"
test -x "$source_directory/buzz-cli" || fail "package is missing bundled Buzz CLI"
release="/opt/buzz-server/releases/$version-$target"
previous=$(readlink -f /opt/buzz-server/current 2>/dev/null || true)
log "Preparing system accounts and directories"
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
  /var/lib/buzz-server/runtime/agent \
  /var/lib/buzz-server/runtime/agent/tmp
install -d -o buzz-agent -g buzz-server -m 0710 /var/lib/buzz-server/runtime
install -d -o root -g root -m 0755 /opt/buzz-server /opt/buzz-server/releases
[ ! -e "$release" ] && [ ! -L "$release" ] || {
  echo "release $version-$target is already installed; immutable releases are never overwritten" >&2
  exit 73
}
install -d -o root -g buzz-server -m 0750 /etc/buzz-server
install -d -o root -g buzz-server -m 0755 /var/lib/buzz-server
install -d -o buzz-server -g buzz-server -m 0700 /var/log/buzz-server
install -d -o root -g root -m 0755 /usr/libexec/buzz-server
log "Staging immutable release $version-$target"
release_staging=$(mktemp -d "/opt/buzz-server/releases/.${version}-${target}.staging.XXXXXX")
install -o root -g root -m 0555 "$source_directory/buzz-server" "$release_staging/buzz-server"
install -o root -g root -m 0555 "$source_directory/buzz-server-daemon" "$release_staging/buzz-server-daemon"
install -o root -g root -m 0555 "$source_directory/buzz-agentctl" "$release_staging/buzz-agentctl"
install -o root -g root -m 0555 "$source_directory/buzz-secretsctl" "$release_staging/buzz-secretsctl"
install -o root -g root -m 0555 "$source_directory/buzz-runtime-probe" "$release_staging/buzz-runtime-probe"
install -o root -g root -m 0555 "$source_directory/buzz-cli" "$release_staging/buzz-cli"
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
log "Preparing configuration and credentials"
if [ ! -e /etc/buzz-server/config.json ]; then
  config_source=${BUZZ_CONFIG_FILE:-$source_directory/config/buzz-server.dev.example.json}
  test -f "$config_source"
  install -o root -g buzz-server -m 0640 "$config_source" /etc/buzz-server/config.json
fi
config_migrated=false
config_backup="$temporary/config.json.previous"
cp -p /etc/buzz-server/config.json "$config_backup"
if python3 - /etc/buzz-server/config.json <<'PYMIGRATE'
import json
import os
import sys
from pathlib import Path

path = Path(sys.argv[1])
config = json.loads(path.read_text())
changed = False

if config.get("owner_secret_file") == "/run/credentials/buzz-server.service/owner-secret":
    config["owner_secret_file"] = "/run/buzz-server/credentials/owner-secret"
    changed = True
if config.get("signer_socket") == "/run/buzz-server/signer.sock":
    config["signer_socket"] = "/run/buzz-server/signer/signer.sock"
    changed = True

probe = {
    "timeout_seconds": 15,
    "command": "/opt/buzz-server/current/buzz-runtime-probe",
    "arguments": [
        "codex-acp-version",
        "/opt/buzz-server/runtimes/codex-acp-1.1.7/bin/codex-acp",
    ],
}
for runtime in config.get("runtime_catalog", {}).get("runtimes", []):
    if runtime.get("id") == "codex-acp" and runtime.get("preflight") != probe:
        runtime["preflight"] = probe
        changed = True

if not changed:
    raise SystemExit(3)

temporary = path.with_name(path.name + ".migrating")
temporary.write_text(json.dumps(config, indent=2) + "\n")
os.chmod(temporary, 0o640)
os.replace(temporary, path)
PYMIGRATE
then
  chown root:buzz-server /etc/buzz-server/config.json
  chmod 0640 /etc/buzz-server/config.json
  config_migrated=true
else
  migration_status=$?
  [ "$migration_status" -eq 3 ] || {
    install -o root -g buzz-server -m 0640 "$config_backup" /etc/buzz-server/config.json
    fail "failed to migrate Buzz Server configuration"
  }
  rm -f "$config_backup"
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
      run_bounded 60 "Encrypting owner secret with AWS KMS" "$release_staging/buzz-secretsctl" encrypt --kms-key-id "$BUZZ_KMS_KEY_ID" --input "$owner_source" --output "$owner_envelope"
      chown root:root "$owner_envelope"
      chmod 0400 "$owner_envelope"
    else
      run_bounded 30 "Persisting owner secret" "$release_staging/buzz-secretsctl" persist --input "$owner_source" --key-file "$owner_key_file" --marker "$owner_marker"
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
    run_bounded 300 "Provisioning pinned runtime packages" "$release_staging/share/deploy/provision-runtimes.sh" "$BUZZ_HARNESS_URL" "$BUZZ_HARNESS_SHA256" "$BUZZ_RUNTIME_URL" "$BUZZ_RUNTIME_SHA256"
  else
    echo "pinned runtime assets are absent; provide BUZZ_HARNESS_URL/SHA256 and BUZZ_RUNTIME_URL/SHA256" >&2
    exit 66
  fi
fi
runtime_assets_valid || { echo "pinned runtime asset validation failed" >&2; exit 66; }
log "Running isolated runtime preflight"
timeout --kill-after=5s 15s runuser --user buzz-agent -- /usr/bin/env -i \
  HOME=/var/lib/buzz-server/runtime \
  TMPDIR=/var/lib/buzz-server/runtime/agent/tmp \
  PATH=/usr/local/bin:/usr/bin:/bin \
  "$release_staging/buzz-runtime-probe" codex-acp-version \
  /opt/buzz-server/runtimes/codex-acp-1.1.7/bin/codex-acp >/dev/null || {
    echo "pinned Codex ACP runtime failed the availability/version preflight" >&2
    exit 66
  }
unit_backup="$temporary/buzz-server.service.previous"
health_service_backup="$temporary/buzz-server-healthcheck.service.previous"
health_timer_backup="$temporary/buzz-server-healthcheck.timer.previous"
unit_existed=false
health_service_existed=false
health_timer_existed=false
[ ! -e /etc/systemd/system/buzz-server.service ] || { cp -L /etc/systemd/system/buzz-server.service "$unit_backup"; unit_existed=true; }
[ ! -e /etc/systemd/system/buzz-server-healthcheck.service ] || { cp -L /etc/systemd/system/buzz-server-healthcheck.service "$health_service_backup"; health_service_existed=true; }
[ ! -e /etc/systemd/system/buzz-server-healthcheck.timer ] || { cp -L /etc/systemd/system/buzz-server-healthcheck.timer "$health_timer_backup"; health_timer_existed=true; }

if timeout 5 systemctl cat buzz-server.service >/dev/null 2>&1; then
  drain_service "Stopping existing Buzz Server process tree"
fi

log "Activating release $version-$target"
mv -T "$release_staging" "$release"
release_staging=
ln -sfn "$release" /opt/buzz-server/current.next
mv -Tf /opt/buzz-server/current.next /opt/buzz-server/current
install -o root -g root -m 0444 "$release/share/deploy/buzz-server.service" /etc/systemd/system/buzz-server.service
install -o root -g root -m 0444 "$release/share/deploy/buzz-server-healthcheck.service" /etc/systemd/system/buzz-server-healthcheck.service
install -o root -g root -m 0444 "$release/share/deploy/buzz-server-healthcheck.timer" /etc/systemd/system/buzz-server-healthcheck.timer
ln -sfn /opt/buzz-server/current/share/deploy/install-release.sh /usr/libexec/buzz-server/install-release.sh
ln -sfn /opt/buzz-server/current/buzz-server /usr/local/bin/buzz-server
rm -f /usr/local/sbin/buzz-serverctl /usr/local/bin/buzz-agentctl /usr/local/sbin/buzz-secretsctl
run_bounded 20 "Reloading systemd configuration" systemctl daemon-reload
run_bounded 20 "Enabling Buzz Server services" systemctl enable buzz-server.service buzz-server-healthcheck.timer
run_bounded 20 "Starting health-check timer" systemctl --no-block restart buzz-server-healthcheck.timer

new_controller="$release/share/deploy/buzz-serverctl"
log "Starting Buzz Server"
if ! timeout 20 systemctl --no-block restart buzz-server.service; then
  service_diagnostics
  deployment_ok=false
elif wait_for_health "$new_controller" "Buzz Server $version" 30; then
  deployment_ok=true
else
  service_diagnostics
  deployment_ok=false
fi

if [ "$deployment_ok" != true ]; then
  log "Deployment failed; rolling back"
  drain_service "Stopping failed Buzz Server process tree"
  if [ "$config_migrated" = true ]; then
    install -o root -g buzz-server -m 0640 "$config_backup" /etc/buzz-server/config.json
  fi
  if [ -n "$previous" ] && [ -x "$previous/buzz-server" ]; then
    ln -sfn "$previous" /opt/buzz-server/current.next
    mv -Tf /opt/buzz-server/current.next /opt/buzz-server/current
    if [ "$unit_existed" = true ]; then install -o root -g root -m 0444 "$unit_backup" /etc/systemd/system/buzz-server.service; else rm -f /etc/systemd/system/buzz-server.service; fi
    if [ "$health_service_existed" = true ]; then install -o root -g root -m 0444 "$health_service_backup" /etc/systemd/system/buzz-server-healthcheck.service; else rm -f /etc/systemd/system/buzz-server-healthcheck.service; fi
    if [ "$health_timer_existed" = true ]; then install -o root -g root -m 0444 "$health_timer_backup" /etc/systemd/system/buzz-server-healthcheck.timer; else rm -f /etc/systemd/system/buzz-server-healthcheck.timer; fi
    run_bounded 20 "Reloading systemd configuration after rollback" systemctl daemon-reload
    previous_controller="$previous/share/deploy/buzz-serverctl"
    log "Restarting previous Buzz Server release"
    if ! timeout 20 systemctl --no-block restart buzz-server.service || ! wait_for_health "$previous_controller" "previous Buzz Server release" 30; then
      service_diagnostics
      fail "deployment failed and previous release could not be restored"
    fi
    if [ "$health_timer_existed" = true ]; then timeout 20 systemctl --no-block restart buzz-server-healthcheck.timer >/dev/null 2>&1 || true; fi
    fail "deployment failed; previous release restored"
  fi
  rm -f /opt/buzz-server/current
  timeout 20 systemctl --no-block stop buzz-server.service >/dev/null 2>&1 || true
  fail "deployment failed; no previous release was available"
fi

log "Buzz Server $version installed successfully"
