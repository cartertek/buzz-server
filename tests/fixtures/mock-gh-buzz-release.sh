#!/usr/bin/env bash
set -euo pipefail

case "${MOCK_BUZZ_RELEASE_CASE:-success}" in
  api-failure)
    exit 1
    ;;
esac

case "${1:-}" in
  api)
    endpoint=${2:-}
    case "$endpoint" in
      repos/block/buzz/releases/latest)
        case "${MOCK_BUZZ_RELEASE_CASE:-success}" in
          malformed) printf '{}\n' ;;
          wrong-tag) printf '%s\n' '{"draft":false,"name":"Buzz Relay v1","prerelease":false,"published_at":"2026-08-15T01:09:59Z","tag_name":"v1.0.0"}' ;;
          *) printf '%s\n' '{"draft":false,"name":"Buzz Desktop v0.5.14","prerelease":false,"published_at":"2026-08-15T01:09:59Z","tag_name":"desktop-v0.5.14"}' ;;
        esac
        ;;
      repos/block/buzz/git/ref/tags/desktop-v0.5.14)
        if [ "${MOCK_BUZZ_RELEASE_CASE:-success}" = annotated ]; then
          printf '%s\n' '{"object":{"sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","type":"tag"},"ref":"refs/tags/desktop-v0.5.14"}'
        elif [ "${MOCK_BUZZ_RELEASE_CASE:-success}" = mismatch ]; then
          printf '%s\n' '{"object":{"sha":"1111111111111111111111111111111111111111","type":"commit"},"ref":"refs/tags/desktop-v0.5.14"}'
        else
          printf '%s\n' '{"object":{"sha":"391495e7d347d20b67e39e3c240d17ef63c5c2c0","type":"commit"},"ref":"refs/tags/desktop-v0.5.14"}'
        fi
        ;;
      repos/block/buzz/git/tags/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)
        printf '%s\n' '{"object":{"sha":"391495e7d347d20b67e39e3c240d17ef63c5c2c0","type":"commit"}}'
        ;;
      *)
        exit 1
        ;;
    esac
    ;;
  *)
    exit 64
    ;;
esac
