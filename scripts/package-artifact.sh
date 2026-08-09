#!/bin/sh
set -eu

if [ "$#" -ne 4 ]; then
  echo "usage: package-artifact.sh PAYLOAD_DIR IDENTITY TARGET OUTPUT_DIR" >&2
  exit 64
fi

payload_dir=$1
identity=$2
target=$3
output_dir=$4

case "$identity" in *[!A-Za-z0-9._-]*|'') echo "identity contains unsafe characters" >&2; exit 64 ;; esac
case "$target" in x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu) ;; *) echo "unsupported target" >&2; exit 64 ;; esac

package=buzz-server
stage="$output_dir/$package"
rm -rf "$stage"
mkdir -p "$stage/config" "$stage/deploy"

for file in buzz-server-daemon buzz-agentctl buzz-secretsctl buzz-runtime-probe buzz-cli; do
  install -m 0755 "$payload_dir/$file" "$stage/$file"
done
sed "s/@BUZZ_SERVER_IDENTITY@/${identity}/g" "$payload_dir/buzz-server.in" > "$stage/buzz-server"
chmod 0755 "$stage/buzz-server"
install -m 0644 "$payload_dir/config/buzz-server.dev.example.json" "$payload_dir/config/buzz-server.schema.json" "$stage/config/"
install -m 0644 "$payload_dir/deploy/README.md" "$payload_dir/deploy/"*.service "$payload_dir/deploy/"*.timer "$stage/deploy/"
sed -e "s/@BUZZ_SERVER_IDENTITY@/${identity}/g" -e "s/@BUZZ_SERVER_TARGET@/${target}/g" "$payload_dir/deploy/install.sh.in" > "$stage/deploy/install.sh"
chmod 0755 "$stage/deploy/install.sh"
for file in install-package.sh install-release.sh buzz-serverctl provision-runtimes.sh prepare-owner-credential.sh backup.sh restore.sh rotate-owner.sh healthcheck.sh disaster-recovery-exercise.sh; do
  install -m 0755 "$payload_dir/deploy/$file" "$stage/deploy/$file"
done

asset="buzz-server-${target}.tar.gz"
tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner -C "$output_dir" -cf - "$package" | gzip -n > "$output_dir/$asset"
(cd "$output_dir" && sha256sum "$asset" > "$asset.sha256")
