#!/bin/sh
set -eu

[ "$#" -eq 4 ] || {
  echo "usage: provision-runtimes.sh HARNESS_ARCHIVE_URL HARNESS_SHA256 RUNTIME_ARCHIVE_URL RUNTIME_SHA256" >&2
  exit 64
}
harness_url=$1
harness_sha=$2
runtime_url=$3
runtime_sha=$4
case "$harness_url $runtime_url" in https://*' https://'*) ;; *) echo "runtime archive URLs must use HTTPS" >&2; exit 64;; esac
case "$harness_sha$runtime_sha" in *[!0-9a-f]*) echo "runtime checksums must be lowercase hex" >&2; exit 64;; esac
[ "${#harness_sha}" -eq 64 ] && [ "${#runtime_sha}" -eq 64 ] || { echo "runtime checksums must be SHA-256 hex" >&2; exit 64; }
getent group buzz-agent >/dev/null 2>&1 || { echo "buzz-agent group must exist before provisioning" >&2; exit 66; }

temporary=$(mktemp -d)
staging_one=/opt/buzz-server/runtimes/.staging-sprig-0.1.0-$$
staging_two=/opt/buzz-server/runtimes/.staging-codex-acp-1.1.7-$$
cleanup() { rm -rf "$temporary" "$staging_one" "$staging_two"; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' HUP TERM
install -d -o root -g buzz-agent -m 0750 /opt/buzz-server/runtimes

install_archive() {
  name=$1
  url=$2
  digest=$3
  entrypoint=$4
  archive="$temporary/$name.tar.gz"
  case "$name" in sprig-0.1.0) extract=$staging_one;; codex-acp-1.1.7) extract=$staging_two;; *) exit 64;; esac
  target="/opt/buzz-server/runtimes/$name"
  if [ -e "$target" ]; then
    [ -x "$target/$entrypoint" ] &&
      [ "$(cat "$target/.package.sha256" 2>/dev/null)" = "$digest  .package.tar.gz" ] &&
      (cd "$target" && sha256sum -c .package.sha256 >/dev/null) && return 0
    echo "immutable runtime target already exists but does not match requested archive: $target" >&2
    exit 66
  fi
  printf 'Downloading %s...\n' "$name" >&2
  curl --fail --location --proto '=https' --tlsv1.2 --connect-timeout 10 --max-time 180 -o "$archive" "$url" || {
    echo "runtime download failed: $name" >&2
    exit 69
  }
  printf '%s  %s.tar.gz\n' "$digest" "$name" > "$temporary/$name.sha256"
  (cd "$temporary" && sha256sum -c "$name.sha256")
  tar -tzf "$archive" | while IFS= read -r member; do
    case "$member" in "$name/"|"$name/"*) ;; *) echo "unexpected runtime archive member: $member" >&2; exit 65;; esac
    case "/$member/" in */../*) echo "unsafe runtime archive traversal" >&2; exit 65;; esac
  done
  if tar -tvzf "$archive" | awk 'substr($1, 1, 1) !~ /^[-d]$/ { found=1 } END { exit found ? 0 : 1 }'; then
    echo "runtime archive must contain only regular files and directories" >&2
    exit 65
  fi
  mkdir -m 0750 "$extract"
  tar --no-same-owner --no-same-permissions -C "$extract" -xzf "$archive"
  test -x "$extract/$name/$entrypoint" || { echo "runtime archive lacks executable $entrypoint" >&2; exit 65; }
  chown -R root:buzz-agent "$extract/$name"
  chmod -R g+rX,g-w,o-rwx "$extract/$name"
  install -o root -g buzz-agent -m 0640 "$archive" "$extract/$name/.package.tar.gz"
  printf '%s  .package.tar.gz\n' "$digest" > "$extract/$name/.package.sha256"
  chown root:buzz-agent "$extract/$name/.package.sha256"
  chmod 0640 "$extract/$name/.package.sha256"
  mv "$extract/$name" "$target"
  rmdir "$extract"
}

install_archive sprig-0.1.0 "$harness_url" "$harness_sha" bin/buzz-acp
install_archive codex-acp-1.1.7 "$runtime_url" "$runtime_sha" bin/codex-acp
