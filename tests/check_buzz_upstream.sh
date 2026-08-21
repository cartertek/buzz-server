#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
mock="$repo_root/tests/fixtures/mock-gh-buzz-release.sh"
check="$repo_root/scripts/check-buzz-upstream.sh"

success=$(GH_BIN="$mock" MOCK_BUZZ_RELEASE_CASE=success "$check")
grep -Fx 'release_tag=desktop-v0.5.18' <<<"$success"
grep -Fx 'expected_release_commit=39f8b46935736334cdd7045a4e4b5d7eb1a33888' <<<"$success"

annotated=$(GH_BIN="$mock" MOCK_BUZZ_RELEASE_CASE=annotated "$check")
grep -Fx 'expected_release_commit=39f8b46935736334cdd7045a4e4b5d7eb1a33888' <<<"$annotated"

if mismatch=$(GH_BIN="$mock" MOCK_BUZZ_RELEASE_CASE=mismatch "$check" 2>&1); then
  echo 'revision mismatch unexpectedly passed' >&2
  exit 1
fi
grep -F 'pinned=39f8b46935736334cdd7045a4e4b5d7eb1a33888' <<<"$mismatch"
grep -F 'release_tag=desktop-v0.5.18' <<<"$mismatch"
grep -F 'expected_release_commit=1111111111111111111111111111111111111111' <<<"$mismatch"

if GH_BIN="$mock" MOCK_BUZZ_RELEASE_CASE=malformed "$check" >/dev/null 2>&1; then
  echo 'malformed release response unexpectedly passed' >&2
  exit 1
fi
if GH_BIN="$mock" MOCK_BUZZ_RELEASE_CASE=api-failure "$check" >/dev/null 2>&1; then
  echo 'API failure unexpectedly passed' >&2
  exit 1
fi
if GH_BIN="$mock" MOCK_BUZZ_RELEASE_CASE=wrong-tag "$check" >/dev/null 2>&1; then
  echo 'non-Desktop release tag unexpectedly passed' >&2
  exit 1
fi

echo 'check-buzz-upstream fixtures: pass'
