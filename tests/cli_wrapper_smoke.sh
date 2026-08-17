#!/bin/sh
set -eu
root=${BUZZ_SERVER_SMOKE_ROOT:-}
[ -n "$root" ] || { echo "SKIP: set BUZZ_SERVER_SMOKE_ROOT to run packaged wrapper smoke"; exit 0; }
mkdir -p "$root/community-identities"
printf '%s' secret >"$root/community-identities/pubkey.secret"
release=$(mktemp -d)
trap 'rm -rf "$release" "$root"' EXIT
cp deploy/buzz-server "$release/buzz-server"
sed -i "s#/run/buzz-server/community-identities/#$root/community-identities/#" "$release/buzz-server"
printf '%s\n' '#!/bin/sh' 'exit 0' >"$release/buzz-server-daemon"
printf '%s\n' '#!/bin/sh' 'case "$1" in community-relay) printf "%s\\n" ws://relay.test/ ;; community-identity-pubkey) printf "%s\\n" pubkey ;; esac' >"$release/buzz-agentctl"
printf '%s\n' '#!/bin/sh' '[ "$1" = messages ] && [ "$2" = send ] && [ "$3" = --channel ] && [ "$4" = chan ] && [ "$5" = --content ] && [ "$6" = "hello world" ]' 'printf "%s\\n" "$*"' 'printf "%s\\n" delegated >&2' 'exit 23' >"$release/buzz-cli"
printf '%s\n' '#!/bin/sh' '[ "$1" = subscribe ]' 'printf "%s\\n" "{\"type\":\"eose\"}"' 'printf "%s\\n" "{\"type\":\"event\",\"event\":{\"id\":\"after-eose\"}}"' 'trap "exit 0" TERM INT' 'while :; do sleep 1; done' >"$release/buzz-events"
chmod +x "$release"/*
set +e
output=$("$release/buzz-server" messages send --community community_test --channel chan --content 'hello world' 2>"$release/err")
status=$?
set -e
[ "$status" -eq 23 ]
[ "$output" = 'messages send --channel chan --content hello world' ]
[ "$(cat "$release/err")" = delegated ]
events_output=$("$release/buzz-server" events subscribe --community community_test & pid=$!; sleep 0.1; kill -TERM "$pid"; wait "$pid" || exit 1)
printf '%s\n' "$events_output" | grep -q after-eose
echo 'PASS: positional community, delegated argv/stdio/exit, packaged events discovery, post-EOSE output, SIGTERM'
