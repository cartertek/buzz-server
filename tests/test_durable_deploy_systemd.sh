#!/bin/sh
set -eu

test_root=/var/lib/buzz-server/runtime/deploy-fixture.$$
main_unit=buzz-server-fixture-main-$$.service
detached_unit=buzz-server-fixture-detached-$$.service
caller_pid=
cleanup() {
  systemctl stop "$main_unit" "$detached_unit" >/dev/null 2>&1 || :
  systemctl reset-failed "$main_unit" "$detached_unit" >/dev/null 2>&1 || :
  rm -rf "$test_root"
}
trap cleanup EXIT INT TERM

install -d -o root -g root -m 0700 "$test_root"
cat >"$test_root/detached.sh" <<'EOF_DETACHED'
#!/bin/sh
set -eu
sleep 2
printf 'completed\n' >"$TEST_ROOT/completed"
EOF_DETACHED
chmod 0555 "$test_root/detached.sh"

cat >"$test_root/caller.sh" <<EOF_CALLER
#!/bin/sh
set -eu
printf '%s\n' "\$\$" > "$test_root/caller.pid"
echo "phase: caller requests detached deployment"
systemd-run --quiet --no-block --unit="$detached_unit" --collect --service-type=oneshot \\
  --setenv=TEST_ROOT="$test_root" "$test_root/detached.sh"
printf 'accepted\n' > "$test_root/accepted"
sleep 300
EOF_CALLER
chmod 0555 "$test_root/caller.sh"

echo "phase: start managed-agent descendant"
systemd-run --quiet --no-block --unit="$main_unit" --service-type=simple \
  --property=KillMode=control-group --property=PrivateTmp=true \
  "$test_root/caller.sh"
for _ in 1 2 3 4 5; do
  [ -f "$test_root/accepted" ] && break
  sleep 1
done
[ -f "$test_root/accepted" ]
[ -f "$test_root/caller.pid" ]
caller_pid=$(cat "$test_root/caller.pid")

# The requester is a descendant of the main service and must be terminated
# with that service. The detached deployment must remain outside its cgroup.
echo "phase: stop main service and assert caller is killed"
systemctl stop "$main_unit"
if kill -0 "$caller_pid" 2>/dev/null; then
  echo "managed deployment caller survived service stop" >&2
  exit 1
fi

for _ in 1 2 3 4 5 6 7 8 9 10; do
  [ -f "$test_root/completed" ] && break
  sleep 1
done
 [ -f "$test_root/completed" ]
echo "assertion: detached deployment completed after requester termination"
