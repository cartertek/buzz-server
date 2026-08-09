#!/bin/sh
set -eu
: "${RELEASE_TEST_STATE:?RELEASE_TEST_STATE is required}"
: "${RELEASE_TEST_SOURCE_SHA:?RELEASE_TEST_SOURCE_SHA is required}"
: "${RELEASE_REAL_GH:?RELEASE_REAL_GH is required}"
state=$RELEASE_TEST_STATE
mkdir -p "$state/assets"

[ "$#" -gt 0 ] || exit 64
command=$1
shift

case "$command" in
  api)
    # Preserve real repository reads used by release resolution, except make the
    # source SHA appear as current master so the production safety check runs.
    case "${1:-}" in
      repos/*/git/ref/heads/master)
        printf '%s\n' "$RELEASE_TEST_SOURCE_SHA"
        exit 0
        ;;
      repos/*/git/matching-refs/tags/v)
        exec "$RELEASE_REAL_GH" api "$@"
        ;;
      repos/*/git/ref/tags/*)
        if [ -f "$state/tag-sha" ]; then
          cat "$state/tag-sha"
          exit 0
        fi
        exec "$RELEASE_REAL_GH" api "$@"
        ;;
    esac

    method=GET
    endpoint=
    sha=
    ref=
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --method) method=$2; shift 2 ;;
        -f)
          case "$2" in sha=*) sha=${2#sha=} ;; ref=*) ref=${2#ref=} ;; esac
          shift 2
          ;;
        --jq) shift 2 ;;
        -H|--header) shift 2 ;;
        --paginate) shift ;;
        *) [ -n "$endpoint" ] || endpoint=$1; shift ;;
      esac
    done
    case "$method:$endpoint" in
      POST:repos/*/git/refs)
        [ -n "$ref" ] && [ -n "$sha" ] || { echo "mock gh: missing ref or sha" >&2; exit 64; }
        printf '%s\n' "$sha" > "$state/tag-sha"
        ;;
      *) echo "mock gh: unsupported api call: $method $endpoint" >&2; exit 64 ;;
    esac
    ;;
  release)
    sub=$1
    shift
    case "$sub" in
      view)
        tag=$1; shift
        [ -f "$state/release-state" ] || exit 1
        if [ "${1:-}" = --json ]; then
          [ "$2" = isDraft ] && [ "$3" = --jq ] && [ "$4" = .isDraft ] || exit 64
          if [ "$(cat "$state/release-state")" = draft ]; then printf 'true\n'; else printf 'false\n'; fi
        fi
        ;;
      create)
        tag=$1; shift
        [ -f "$state/tag-sha" ] || { echo "mock gh: --verify-tag failed" >&2; exit 1; }
        printf 'draft\n' > "$state/release-state"
        ;;
      download)
        tag=$1; shift
        [ -f "$state/release-state" ] || exit 1
        pattern=
        destination=
        while [ "$#" -gt 0 ]; do
          case "$1" in
            --pattern) pattern=$2; shift 2 ;;
            --dir) destination=$2; shift 2 ;;
            *) echo "mock gh: unsupported release download arg: $1" >&2; exit 64 ;;
          esac
        done
        [ -n "$destination" ] || exit 64
        mkdir -p "$destination"
        if [ -n "$pattern" ]; then
          [ -f "$state/assets/$pattern" ] || exit 1
          cp "$state/assets/$pattern" "$destination/$pattern"
        else
          found=false
          for asset in "$state/assets"/*; do
            [ -f "$asset" ] || continue
            cp "$asset" "$destination/$(basename "$asset")"
            found=true
          done
          [ "$found" = true ] || exit 1
        fi
        ;;
      upload)
        tag=$1; asset=$2
        [ -f "$state/release-state" ] || exit 1
        [ -f "$asset" ] || { echo "mock gh: upload source is not a regular file: $asset" >&2; exit 1; }
        cp "$asset" "$state/assets/$(basename "$asset")"
        ;;
      edit)
        tag=$1; shift
        [ -f "$state/release-state" ] || exit 1
        [ "${1:-}" = --draft=false ] || { echo "mock gh: unsupported release edit" >&2; exit 64; }
        printf 'published\n' > "$state/release-state"
        ;;
      *) echo "mock gh: unsupported release command: $sub" >&2; exit 64 ;;
    esac
    ;;
  *) echo "mock gh: unsupported command: $command" >&2; exit 64 ;;
esac
