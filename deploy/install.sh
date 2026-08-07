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
    prompt BUZZ_RELAY_URL "Buzz relay URL"
    prompt BUZZ_AGENT_SECRET "Agent Nostr secret" true
    prompt BUZZ_AGENT_PUBKEY "Agent Nostr public key (hex)"
    prompt BUZZ_OWNER_SECRET "Owner Nostr secret" true
    prompt BUZZ_OPENAI_API_KEY "OpenAI API key" true
    prompt BUZZ_HARNESS_URL "Sprig package URL"
    prompt BUZZ_HARNESS_SHA256 "Sprig package SHA-256"
    prompt BUZZ_RUNTIME_URL "Codex ACP package URL"
    prompt BUZZ_RUNTIME_SHA256 "Codex ACP package SHA-256"

    community_id=$(new_id community_)
    agent_id=$(new_id agent_)
    BUZZ_CONFIG_FILE="$temporary/config.json"
    BUZZ_SECRETS_FILE="$temporary/secrets.env"
    BUZZ_OWNER_SECRET_FILE="$temporary/owner-secret"
    export BUZZ_CONFIG_FILE BUZZ_SECRETS_FILE BUZZ_OWNER_SECRET_FILE

    cat >"$BUZZ_CONFIG_FILE" <<EOF_CONFIG
{
  "state_database": "/var/lib/buzz-server/state.sqlite3",
  "receipt_file": "/var/lib/buzz-server/process-receipt.json",
  "signer_socket": "/run/buzz-server/signer/signer.sock",
  "log_directory": "/var/log/buzz-server/agents",
  "working_directory": "/var/lib/buzz-server",
  "workspace_path": "/var/lib/buzz-server/workspaces/agent",
  "runtime_path": "/var/lib/buzz-server/runtime/agent",
  "agent_secret_env": "BUZZ_AGENT_SECRET",
  "owner_secret_file": "/run/buzz-server/credentials/owner-secret",
  "runtime_user": "buzz-agent",
  "expected_agent_pubkey": "$BUZZ_AGENT_PUBKEY",
  "signer_conditions": "kind=9",
  "community": {
    "id": "$community_id",
    "display_name": "Buzz",
    "relay_url": "$BUZZ_RELAY_URL"
  },
  "agent": {
    "id": "$agent_id",
    "community_config_id": "$community_id",
    "display_name": "Buzz agent",
    "system_prompt": "You are a Buzz agent.",
    "runtime": {"runtime_id": "codex-acp", "environment": {"RUST_LOG": "info"}},
    "desired_state": "enabled"
  },
  "runtime_catalog": {
    "runtimes": [{
      "id": "codex-acp",
      "version": "1.1.7",
      "artifact": {"kind": "package", "manager": "npm", "name": "codex-acp", "version": "1.1.7"},
      "command": "/opt/buzz-server/runtimes/codex-acp-1.1.7/bin/codex-acp",
      "arguments": ["acp"],
      "preflight": {
        "timeout_seconds": 15,
        "command": "/opt/buzz-server/runtimes/sprig-0.1.0/bin/buzz-acp",
        "arguments": ["models", "--json", "--agent-command", "/opt/buzz-server/runtimes/codex-acp-1.1.7/bin/codex-acp", "--agent-args", "acp"]
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
    printf 'BUZZ_AGENT_SECRET=%s\nBUZZ_SECRET_OPENAI_API_KEY=%s\n' "$BUZZ_AGENT_SECRET" "$BUZZ_OPENAI_API_KEY" >"$BUZZ_SECRETS_FILE"
    printf '%s\n' "$BUZZ_OWNER_SECRET" >"$BUZZ_OWNER_SECRET_FILE"
    chmod 0600 "$BUZZ_CONFIG_FILE" "$BUZZ_SECRETS_FILE" "$BUZZ_OWNER_SECRET_FILE"
  fi
fi

script_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
package_directory=$(dirname "$script_directory")
BUZZ_RELEASE_SOURCE_DIR="$package_directory"
export BUZZ_RELEASE_SOURCE_DIR
"$script_directory/install-release.sh" "$version" "$target" "$repository"
