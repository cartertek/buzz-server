#!/bin/sh
set -eu

if [ "$#" -ne 3 ]; then
  echo "usage: resolve-release.sh OWNER/REPOSITORY SOURCE_SHA BUMP" >&2
  exit 64
fi

repository=$1
source_sha=$2
bump=$3
case "$repository" in *[!A-Za-z0-9._/-]*|*/*/*|'') echo "invalid repository" >&2; exit 64 ;; esac
case "$source_sha" in *[!0-9A-Fa-f]*|'') echo "invalid source SHA" >&2; exit 64 ;; esac
[ "${#source_sha}" -eq 40 ] || { echo "invalid source SHA" >&2; exit 64; }
case "$bump" in patch|minor|major) ;; *) echo "unsupported version bump: $bump" >&2; exit 64 ;; esac
command -v gh >/dev/null 2>&1 || { echo "required command not found: gh" >&2; exit 69; }

remote_master=$(gh api "repos/${repository}/git/ref/heads/master" --jq .object.sha)
[ "$source_sha" = "$remote_master" ] || {
  echo "release source ${source_sha} is no longer current master ${remote_master}" >&2
  exit 1
}

latest=$(gh api "repos/${repository}/git/matching-refs/tags/v" --paginate --jq \
  '[.[] | .ref | sub("^refs/tags/v"; "") | select(test("^[0-9]+\\.[0-9]+\\.[0-9]+$")) | split(".") | map(tonumber)] | sort | last | @tsv')
if [ -z "$latest" ] || [ "$latest" = null ]; then
  major=0; minor=0; patch=0
else
  set -- $latest
  major=$1; minor=$2; patch=$3
fi
case "$bump" in
  patch) patch=$((patch + 1)) ;;
  minor) minor=$((minor + 1)); patch=0 ;;
  major) major=$((major + 1)); minor=0; patch=0 ;;
esac
tag="v${major}.${minor}.${patch}"

existing=$(scripts/github-tag-sha.sh "$repository" "$tag")
[ -z "$existing" ] || {
  echo "calculated release tag ${tag} is already reserved at ${existing}" >&2
  exit 1
}
printf '%s\n' "$tag"
