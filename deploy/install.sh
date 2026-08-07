#!/bin/sh
set -eu

repository=${BUZZ_SERVER_REPOSITORY:-cartertek/buzz-server}
version=${BUZZ_SERVER_VERSION:-}
target=${BUZZ_SERVER_TARGET:-}
non_interactive=false

usage() {
  cat <<'USAGE'
usage: install.sh [--non-interactive] [--version VERSION] [--target TARGET]

Interactive mode prompts for missing first-install inputs. Non-interactive mode
requires them through the BUZZ_* environment variables documented in README.md.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --non-interactive) non_interactive=true ;;
    --version)
      [ "$#" -ge 2 ] || { usage >&2; exit 64; }
      version=$2
      shift
      ;;
    --target)
      [ "$#" -ge 2 ] || { usage >&2; exit 64; }
      target=$2
      shift
      ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 64 ;;
  esac
  shift
done

case "$repository" in
  *[!A-Za-z0-9._/-]*|*/*/*|'') echo "invalid BUZZ_SERVER_REPOSITORY" >&2; exit 64 ;;
esac

if [ -z "$target" ]; then
  case "$(uname -m)" in
    x86_64|amd64) target=x86_64-unknown-linux-gnu ;;
    aarch64|arm64) target=aarch64-unknown-linux-gnu ;;
    *) echo "unsupported architecture; set BUZZ_SERVER_TARGET" >&2; exit 64 ;;
  esac
fi

if [ -z "$version" ]; then
  latest_url=$(curl --fail --silent --show-error --location --output /dev/null --write-out '%{url_effective}' \
    "https://github.com/${repository}/releases/latest")
  version=${latest_url##*/}
fi
case "$version" in
  v[0-9]*) ;;
  *) echo "BUZZ_SERVER_VERSION must be an immutable v* tag" >&2; exit 64 ;;
esac

get_value() {
  printenv "$1" 2>/dev/null || true
}

prompt_file() {
  variable=$1
  description=$2
  value=$(get_value "$variable")
  if [ -z "$value" ] && [ "$non_interactive" = false ] && [ -t 0 ]; then
    printf '%s: ' "$description" >&2
    IFS= read -r value
    export "$variable=$value"
  fi
  [ -n "$value" ] || { echo "$variable is required for first installation" >&2; exit 66; }
  [ -f "$value" ] || { echo "$variable does not name a file: $value" >&2; exit 66; }
}

prompt_value() {
  variable=$1
  description=$2
  value=$(get_value "$variable")
  if [ -z "$value" ] && [ "$non_interactive" = false ] && [ -t 0 ]; then
    printf '%s: ' "$description" >&2
    IFS= read -r value
    export "$variable=$value"
  fi
  [ -n "$value" ] || { echo "$variable is required for first installation" >&2; exit 66; }
}

if [ ! -f /etc/buzz-server/config.json ]; then
  prompt_file BUZZ_CONFIG_FILE "Configuration JSON file"
  prompt_file BUZZ_SECRETS_FILE "Runtime secrets environment file"
  prompt_file BUZZ_OWNER_SECRET_FILE "Owner Nostr secret file"
  prompt_value BUZZ_HARNESS_URL "Sprig package HTTPS URL"
  prompt_value BUZZ_HARNESS_SHA256 "Sprig package SHA-256"
  prompt_value BUZZ_RUNTIME_URL "Codex ACP package HTTPS URL"
  prompt_value BUZZ_RUNTIME_SHA256 "Codex ACP package SHA-256"
fi

temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
installer_url="https://raw.githubusercontent.com/${repository}/${version}/deploy/install-release.sh"
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
  --output "$temporary/install-release.sh" "$installer_url"
chmod 0755 "$temporary/install-release.sh"

exec "$temporary/install-release.sh" "$version" "$target" "$repository"
