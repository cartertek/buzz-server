#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
  echo "usage: github-tag-sha.sh OWNER/REPOSITORY TAG" >&2
  exit 64
fi

repository=$1
tag=$2
case "$repository" in *[!A-Za-z0-9._/-]*|*/*/*|'') echo "invalid repository" >&2; exit 64 ;; esac
case "$tag" in *[!A-Za-z0-9._-]*|'') echo "invalid tag" >&2; exit 64 ;; esac
command -v gh >/dev/null 2>&1 || { echo "required command not found: gh" >&2; exit 69; }

sha=
if value=$(gh api "repos/${repository}/git/ref/tags/${tag}" --jq .object.sha 2>/dev/null); then
  sha=$value
fi
case "$sha" in
  '') ;;
  *[!0-9A-Fa-f]* ) echo "GitHub returned an invalid tag SHA" >&2; exit 65 ;;
esac
[ -z "$sha" ] || [ "${#sha}" -eq 40 ] || { echo "GitHub returned an invalid tag SHA" >&2; exit 65; }
printf '%s\n' "$sha"
