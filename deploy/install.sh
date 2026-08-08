#!/bin/sh
set -eu

repository=${BUZZ_SERVER_REPOSITORY:-cartertek/buzz-server}
version='@BUZZ_SERVER_VERSION@'
target='@BUZZ_SERVER_TARGET@'
non_interactive=false

usage() {
  cat <<'USAGE'
usage: install.sh [--non-interactive]
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --non-interactive) non_interactive=true ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 64 ;;
  esac
  shift
done

case "$version" in
  v[0-9]*) ;;
  *) echo "this installer is not bound to a Buzz Server release" >&2; exit 64 ;;
esac
case "$repository" in
  *[!A-Za-z0-9._/-]*|*/*/*|'') echo "invalid BUZZ_SERVER_REPOSITORY" >&2; exit 64 ;;
esac

case "$(uname -m)" in
  x86_64|amd64) host_target=x86_64-unknown-linux-gnu ;;
  aarch64|arm64) host_target=aarch64-unknown-linux-gnu ;;
  *) echo "unsupported architecture" >&2; exit 64 ;;
esac
[ "$target" = "$host_target" ] || {
  echo "this package is for $target, but this host is $host_target" >&2
  exit 65
}

get_value() {
  printenv "$1" 2>/dev/null || true
}

prompt() {
  variable=$1
  label=$2
  secret=${3:-false}
  value=$(get_value "$variable")
  if [ -z "$value" ] && [ "$non_interactive" = false ] && [ -r /dev/tty ]; then
    printf '%s: ' "$label" >/dev/tty
    if [ "$secret" = true ]; then stty -echo </dev/tty; fi
    IFS= read -r value </dev/tty
    if [ "$secret" = true ]; then stty echo </dev/tty; printf '\n' >/dev/tty; fi
    export "$variable=$value"
  fi
  [ -n "$value" ] || { echo "$variable is required for first installation" >&2; exit 66; }
}

new_id() {
  prefix=$1
  if [ -r /proc/sys/kernel/random/uuid ]; then
    uuid=$(cat /proc/sys/kernel/random/uuid)
  elif command -v uuidgen >/dev/null 2>&1; then
    uuid=$(uuidgen)
  else
    echo "cannot generate installation IDs" >&2
    exit 69
  fi
  printf '%s%s\n' "$prefix" "$(printf '%s' "$uuid" | tr -d '-' | tr 'A-F' 'a-f')"
}

temporary=$(mktemp -d)
cleanup() { rm -rf "$temporary"; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' HUP TERM

if [ ! -f /etc/buzz-server/config.json ]; then
  if [ -z "${BUZZ_CONFIG_FILE:-}" ]; then
    prompt BUZZ_OPENAI_API_KEY "OpenAI API key" true
    prompt BUZZ_HARNESS_URL "Sprig package URL"
    prompt BUZZ_HARNESS_SHA256 "Sprig package SHA-256"
    prompt BUZZ_RUNTIME_URL "Codex ACP package URL"
    prompt BUZZ_RUNTIME_SHA256 "Codex ACP package SHA-256"

    BUZZ_CONFIG_FILE="$temporary/config.json"
    BUZZ_SECRETS_FILE="$temporary/secrets.env"
    export BUZZ_CONFIG_FILE BUZZ_SECRETS_FILE

    cat >"$BUZZ_CONFIG_FILE" <<EOF_CONFIG
{
  "state_database": "/var/lib/buzz-server/state.sqlite3",
  "log_directory": "/var/log/buzz-server/agents",
  "working_directory": "/var/lib/buzz-server",
  "runtime_user": "buzz-agent",
  "signer_conditions": "kind=9",
  "runtime_catalog": {
    "runtimes": [{
      "id": "codex-acp",
      "version": "1.1.7",
      "artifact": {"kind": "package", "manager": "npm", "name": "codex-acp", "version": "1.1.7"},
      "command": "/opt/buzz-server/runtimes/codex-acp-1.1.7/bin/codex-acp",
      "arguments": ["acp"],
      "preflight": {
        "timeout_seconds": 15,
        "command": "/opt/buzz-server/current/buzz-runtime-probe",
        "arguments": ["codex-acp-version", "/opt/buzz-server/runtimes/codex-acp-1.1.7/bin/codex-acp"]
      },
      "required_secrets": [{"environment_key": "OPENAI_API_KEY", "secret_name": "BUZZ_SECRET_OPENAI_API_KEY"}]
    }]
  },
  "harness": {"path": "/opt/buzz-server/runtimes/sprig-0.1.0/bin/buzz-acp", "package_id": "sprig", "version": "0.1.0", "sha256": null},
  "harness_arguments": [],
  "restart": {"mode": "on_failure", "max_attempts": 5, "initial_backoff_ms": 250, "max_backoff_ms": 30000, "stable_after_ms": 60000},
  "health": {"kind": "process", "startup_grace_ms": 5000},
  "lifecycle_api": {"unix_socket": "/run/buzz-server/lifecycle.sock", "administrator_uids": [0], "draft_submitter_uids": [], "retention_seconds": 2592000, "tls": null}
}
EOF_CONFIG
    printf 'BUZZ_SECRET_OPENAI_API_KEY=%s\n' "$BUZZ_OPENAI_API_KEY" >"$BUZZ_SECRETS_FILE"
    chmod 0600 "$BUZZ_CONFIG_FILE" "$BUZZ_SECRETS_FILE"
  fi
fi

script_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
package_directory=$(dirname "$script_directory")
BUZZ_RELEASE_SOURCE_DIR="$package_directory"
export BUZZ_RELEASE_SOURCE_DIR
"$script_directory/install-release.sh" "$version" "$target" "$repository"
