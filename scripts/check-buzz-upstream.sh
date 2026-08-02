#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cargo_toml="$repo_root/Cargo.toml"
pinned=$(sed -n 's/.*buzz-sdk.*rev = "\([0-9a-f]\{40\}\)".*/\1/p' "$cargo_toml")
if [[ ! "$pinned" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: could not resolve the pinned Buzz revision from Cargo.toml" >&2
  exit 2
fi

latest=$(git ls-remote https://github.com/block/buzz.git refs/heads/main | awk '{print $1}')
if [[ ! "$latest" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: could not resolve block/buzz main" >&2
  exit 2
fi

echo "pinned=$pinned"
echo "latest=$latest"
if [[ "$pinned" != "$latest" ]]; then
  echo "::error title=Buzz dependency is stale::Pinned $pinned; current main $latest. Run the compatibility fixtures and review upstream changes before updating the pin."
  exit 1
fi

echo "Buzz dependency pin matches current main."
