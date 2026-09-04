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
          *) printf '%s\n' '{"draft":false,"name":"Buzz Desktop v0.5.22","prerelease":false,"published_at":"2026-09-04T14:38:43Z","tag_name":"desktop-v0.5.22"}' ;;
        esac
        ;;
      repos/block/buzz/git/ref/tags/desktop-v0.5.22)
        if [ "${MOCK_BUZZ_RELEASE_CASE:-success}" = annotated ]; then
          printf '%s\n' '{"object":{"sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","type":"tag"},"ref":"refs/tags/desktop-v0.5.22"}'
        elif [ "${MOCK_BUZZ_RELEASE_CASE:-success}" = mismatch ]; then
          printf '%s\n' '{"object":{"sha":"1111111111111111111111111111111111111111","type":"commit"},"ref":"refs/tags/desktop-v0.5.22"}'
        else
          printf '%s\n' '{"object":{"sha":"9ceb1f79bbc21785a0a075c40aecb3c058b1ea15","type":"commit"},"ref":"refs/tags/desktop-v0.5.22"}'
        fi
        ;;
      repos/block/buzz/git/tags/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)
        printf '%s\n' '{"object":{"sha":"9ceb1f79bbc21785a0a075c40aecb3c058b1ea15","type":"commit"}}'
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
