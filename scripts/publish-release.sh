#!/bin/sh
set -eu

if [ "$#" -ne 3 ]; then
  echo "usage: publish-release.sh TAG SHA DIST_DIR" >&2
  exit 64
fi

tag=$1
sha=$2
dist=$3
: "${GH_REPO:?GH_REPO is required}"

case "$tag" in v[0-9]*) ;; *) echo "invalid release tag" >&2; exit 64 ;; esac
case "$sha" in ????????*) ;; *) echo "invalid source SHA" >&2; exit 64 ;; esac
[ -d "$dist" ] || { echo "release asset directory not found: $dist" >&2; exit 66; }

entry_count=$(find "$dist" -mindepth 1 -maxdepth 1 | wc -l)
file_count=$(find "$dist" -mindepth 1 -maxdepth 1 -type f | wc -l)
[ "$entry_count" -eq 4 ] && [ "$file_count" -eq 4 ] || {
  echo "release asset directory must contain exactly four regular files" >&2
  find "$dist" -mindepth 1 -maxdepth 1 -printf '%y %f\n' >&2
  exit 65
}

for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
  [ -f "$dist/buzz-server-${target}.tar.gz" ] || { echo "missing release archive for $target" >&2; exit 65; }
  [ -f "$dist/buzz-server-${target}.tar.gz.sha256" ] || { echo "missing release checksum for $target" >&2; exit 65; }
done

existing=$(scripts/github-tag-sha.sh "$GH_REPO" "$tag")
if [ -z "$existing" ]; then
  gh api --method POST "repos/${GH_REPO}/git/refs" -f ref="refs/tags/${tag}" -f sha="$sha" >/dev/null
else
  [ "$existing" = "$sha" ] || { echo "tag ${tag} already exists at ${existing}; refusing to move it" >&2; exit 1; }
fi

if gh release view "$tag" >/dev/null 2>&1; then
  draft=$(gh release view "$tag" --json isDraft --jq .isDraft)
  [ "$draft" = true ] || { echo "release ${tag} is already published" >&2; exit 1; }
else
  gh release create "$tag" --draft --verify-tag --generate-notes --title "$tag" >/dev/null
fi

remote=$(mktemp -d)
cleanup() { rm -rf "$remote"; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' HUP TERM

for asset in "$dist"/*; do
  [ -f "$asset" ] || { echo "release asset is not a regular file: $asset" >&2; exit 65; }
  name=$(basename "$asset")
  if gh release download "$tag" --pattern "$name" --dir "$remote" >/dev/null 2>&1; then
    cmp "$asset" "$remote/$name"
    rm -f "$remote/$name"
  else
    gh release upload "$tag" "$asset" >/dev/null
  fi
done

rm -rf "$remote"
mkdir -p "$remote"
gh release download "$tag" --dir "$remote" >/dev/null
[ "$(find "$remote" -mindepth 1 -maxdepth 1 -type f | wc -l)" -eq 4 ] || {
  echo "release does not contain exactly four assets" >&2
  exit 65
}
for asset in "$dist"/*; do
  cmp "$asset" "$remote/$(basename "$asset")"
done

gh release edit "$tag" --draft=false >/dev/null
