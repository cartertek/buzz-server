#!/bin/sh
set -eu

root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
mkdir -p "$root/release"
cp deploy/buzz-server "$root/release/buzz-server"
chmod 0755 "$root/release/buzz-server"
touch "$root/release/buzz-server-daemon"
chmod 0755 "$root/release/buzz-server-daemon"

cat > "$root/release/buzz-agentctl" <<'EOF_AGENTCTL'
#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$TEST_CALLS"
printf '{"command":"%s"}\n' "$1"
if [ "${TEST_FAIL_DISABLE:-0}" = 1 ] && [ "$1" = disable ]; then
  exit 23
fi
if [ "${TEST_FAIL_ENABLE:-0}" = 1 ] && [ "$1" = enable ]; then
  exit 24
fi
EOF_AGENTCTL
chmod 0755 "$root/release/buzz-agentctl"

help=$($root/release/buzz-server agents --help)
printf '%s\n' "$help" | grep -F 'reload         Disable and then re-enable an agent' >/dev/null

output=$(TEST_CALLS="$root/calls" "$root/release/buzz-server" agents reload --agent agent_test --correlation reload-test)
test "$(printf '%s\n' "$output" | wc -l)" -eq 1
test "$output" = '{"command":"enable"}'
test "$(sed -n '1p' "$root/calls")" = 'disable --agent agent_test --correlation reload-test'
test "$(sed -n '2p' "$root/calls")" = 'enable --agent agent_test --correlation reload-test'

: > "$root/calls"
if TEST_CALLS="$root/calls" TEST_FAIL_DISABLE=1 "$root/release/buzz-server" agents reload --agent agent_test; then
  echo 'reload unexpectedly succeeded after disable failure' >&2
  exit 1
fi
test "$(wc -l < "$root/calls")" -eq 1
test "$(sed -n '1p' "$root/calls")" = 'disable --agent agent_test'

: > "$root/calls"
if TEST_CALLS="$root/calls" TEST_FAIL_ENABLE=1 "$root/release/buzz-server" agents reload --agent agent_test; then
  echo 'reload unexpectedly succeeded after enable failure' >&2
  exit 1
fi
test "$(wc -l < "$root/calls")" -eq 2
test "$(sed -n '1p' "$root/calls")" = 'disable --agent agent_test'
test "$(sed -n '2p' "$root/calls")" = 'enable --agent agent_test'
