#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
pinned=$(sed -n 's/.*buzz-sdk.*rev = "\([0-9a-f]\{40\}\)".*/\1/p' "$repo_root/Cargo.toml")
if [[ ! "$pinned" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: could not resolve the pinned Buzz revision" >&2
  exit 2
fi

checkout=${BUZZ_UPSTREAM_CHECKOUT:-}
cleanup=
if [[ -z "$checkout" ]]; then
  checkout=$(mktemp -d)
  cleanup=$checkout
  git -C "$checkout" init --quiet
  git -C "$checkout" remote add origin https://github.com/block/buzz.git
  git -C "$checkout" fetch --quiet --depth=1 origin "$pinned"
  git -C "$checkout" checkout --quiet --detach FETCH_HEAD
fi
trap '[[ -z "$cleanup" ]] || rm -rf -- "$cleanup"' EXIT

actual=$(git -C "$checkout" rev-parse HEAD)
if [[ "$actual" != "$pinned" ]]; then
  echo "error: upstream checkout is $actual, expected $pinned" >&2
  exit 2
fi

upstream_fixtures="$checkout/crates/buzz-backend-kubernetes/tests/fixtures/provider-wire"
local_fixtures="$repo_root/tests/fixtures/provider-wire"
mapfile -t upstream_json < <(cd "$upstream_fixtures" && find . -maxdepth 1 -name '*.json' -printf '%f\n' | sort)
mapfile -t local_json < <(cd "$local_fixtures" && find . -maxdepth 1 -name '*.json' -printf '%f\n' | sort)
if [[ "${upstream_json[*]}" != "${local_json[*]}" ]]; then
  echo "error: vendored provider fixture set differs from pinned Buzz" >&2
  printf 'upstream: %s\nlocal:    %s\n' "${upstream_json[*]}" "${local_json[*]}" >&2
  exit 1
fi
for fixture in "${upstream_json[@]}"; do
  cmp "$upstream_fixtures/$fixture" "$local_fixtures/$fixture"
done

cargo test \
  --manifest-path "$checkout/crates/buzz-backend-kubernetes/Cargo.toml" \
  --locked \
  --test wire_fixtures
