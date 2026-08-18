#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cargo_toml="$repo_root/Cargo.toml"
pinned=$(sed -n 's/.*buzz-sdk.*rev = "\([0-9a-f]\{40\}\)".*/\1/p' "$cargo_toml")
if [[ ! "$pinned" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: could not resolve the pinned Buzz revision from Cargo.toml" >&2
  exit 2
fi

gh_bin=${GH_BIN:-gh}
buzz_repo=${BUZZ_UPSTREAM_REPO:-block/buzz}
if [[ ! "$buzz_repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
  echo "error: invalid Buzz repository: $buzz_repo" >&2
  exit 2
fi

api() {
  "$gh_bin" api "$@"
}

if ! release_json=$(api "repos/${buzz_repo}/releases/latest"); then
  echo "error: could not resolve the latest published Buzz Desktop release from GitHub" >&2
  exit 2
fi

if ! release_tag=$(jq -er '
  if type == "object" and
     (.draft | type) == "boolean" and .draft == false and
     (.prerelease | type) == "boolean" and .prerelease == false and
     (.tag_name | type) == "string" and
     (.name | type) == "string" and
     (.published_at | type) == "string"
  then .tag_name else error end
' <<<"$release_json"); then
  echo "error: GitHub returned a malformed latest Buzz Desktop release response" >&2
  exit 2
fi
release_name=$(jq -er '.name' <<<"$release_json")
published_at=$(jq -er '.published_at' <<<"$release_json")
if [[ ! "$release_tag" =~ ^desktop-v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: latest Buzz release is not a Desktop release tag: $release_tag" >&2
  exit 2
fi

if ! tag_ref_json=$(api "repos/${buzz_repo}/git/ref/tags/${release_tag}"); then
  echo "error: could not resolve Buzz Desktop release tag: $release_tag" >&2
  exit 2
fi
if ! ref=$(jq -er --arg tag "refs/tags/$release_tag" '
  if type == "object" and .ref == $tag and
     (.object | type) == "object" and
     (.object.sha | type) == "string" and
     (.object.type | type) == "string"
  then . else error end
' <<<"$tag_ref_json"); then
  echo "error: malformed GitHub tag reference for Buzz Desktop release: $release_tag" >&2
  exit 2
fi

object_sha=$(jq -er '.object.sha' <<<"$ref")
object_type=$(jq -er '.object.type' <<<"$ref")
depth=0
while [[ "$object_type" == tag ]]; do
  depth=$((depth + 1))
  if (( depth > 8 )); then
    echo "error: Buzz Desktop release tag indirection is too deep: $release_tag" >&2
    exit 2
  fi
  if ! annotated_json=$(api "repos/${buzz_repo}/git/tags/${object_sha}"); then
    echo "error: could not peel annotated Buzz Desktop release tag: $release_tag" >&2
    exit 2
  fi
  if ! annotated=$(jq -er '
    if type == "object" and (.object | type) == "object" and
       (.object.sha | type) == "string" and (.object.type | type) == "string"
    then .object else error end
  ' <<<"$annotated_json"); then
    echo "error: malformed annotated Buzz Desktop release tag: $release_tag" >&2
    exit 2
  fi
  object_sha=$(jq -er '.sha' <<<"$annotated")
  object_type=$(jq -er '.type' <<<"$annotated")
done

if [[ "$object_type" != commit || ! "$object_sha" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: Buzz Desktop release tag does not resolve to a commit: $release_tag" >&2
  exit 2
fi

echo "pinned=$pinned"
echo "release_name=$release_name"
echo "release_tag=$release_tag"
echo "release_published_at=$published_at"
echo "expected_release_commit=$object_sha"
if [[ "$pinned" != "$object_sha" ]]; then
  echo "::error title=Buzz dependency is stale::Pinned $pinned; latest Desktop release $release_tag ($release_name) resolves to $object_sha. Run the compatibility fixtures and review upstream changes before updating the pin."
  exit 1
fi

echo "Buzz dependency pin matches the latest published Desktop release."
