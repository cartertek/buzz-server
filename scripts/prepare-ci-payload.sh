#!/bin/sh
set -eu

if [ "$#" -ne 3 ]; then
  echo "usage: prepare-ci-payload.sh TARGET UPSTREAM_BUZZ_DIR OUTPUT_DIR" >&2
  exit 64
fi

target=$1
upstream_buzz_dir=$2
output_dir=$3

rm -rf "$output_dir"
mkdir -p "$output_dir/config" "$output_dir/deploy"
install -m 0755 "target/${target}/release/buzz-server" "$output_dir/buzz-server-daemon"
install -m 0755 "target/${target}/release/buzz-agentctl" "$output_dir/buzz-agentctl"
install -m 0755 "target/${target}/release/buzz-secretsctl" "$output_dir/buzz-secretsctl"
install -m 0755 "target/${target}/release/buzz-runtime-probe" "$output_dir/buzz-runtime-probe"
install -m 0755 "$upstream_buzz_dir/target/${target}/release/buzz" "$output_dir/buzz-cli"
install -m 0644 config/buzz-server.dev.example.json config/buzz-server.schema.json "$output_dir/config/"
install -m 0755 deploy/buzz-server "$output_dir/buzz-server.in"
install -m 0644 deploy/buzz-server.service deploy/buzz-server-healthcheck.service deploy/buzz-server-healthcheck.timer deploy/README.md "$output_dir/deploy/"
install -m 0755 deploy/install.sh "$output_dir/deploy/install.sh.in"
install -m 0755 deploy/install-package.sh deploy/install-release.sh deploy/buzz-serverctl deploy/provision-runtimes.sh deploy/prepare-community-identities.sh deploy/migrate-legacy-owner.py deploy/backup.sh deploy/restore.sh deploy/healthcheck.sh "$output_dir/deploy/"
