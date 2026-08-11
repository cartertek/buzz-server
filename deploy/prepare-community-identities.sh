#!/bin/sh
set -eu

secretsctl=${BUZZ_SECRETSCTL:-/opt/buzz-server/current/buzz-secretsctl}
store=${BUZZ_COMMUNITY_IDENTITY_STORE:-/var/lib/buzz-server/community-identities}
runtime=${BUZZ_COMMUNITY_IDENTITY_RUNTIME:-/run/buzz-server/community-identities}

install -d -o root -g root -m 0700 "$store" "$runtime"
rm -f "$runtime"/*.secret 2>/dev/null || true

pubkeys=$(
  find "$store" -maxdepth 1 -type f \( -name '*.secret' -o -name '*.keyring' -o -name '*.envelope.json' \) -printf '%f\n' 2>/dev/null |
  sed -e 's/\.envelope\.json$//' -e 's/\.keyring$//' -e 's/\.secret$//' |
  sort -u
)
for pubkey in $pubkeys; do
  case "$pubkey" in
    *[!0-9A-Fa-f]*|'') echo "invalid community identity filename: $pubkey" >&2; exit 65 ;;
  esac
  [ "${#pubkey}" -eq 64 ] || { echo "invalid community identity filename: $pubkey" >&2; exit 65; }
  output="$runtime/$pubkey.secret"
  if [ -f "$store/$pubkey.envelope.json" ]; then
    timeout 60 "$secretsctl" decrypt --input "$store/$pubkey.envelope.json" --output "$output"
  else
    timeout 30 "$secretsctl" materialize \
      --output "$output" \
      --key-file "$store/$pubkey.secret" \
      --marker "$store/$pubkey.keyring" \
      --service buzz-server \
      --name "community-identity:$pubkey"
  fi
  actual=$("$secretsctl" public-key --input "$output")
  [ "$actual" = "$pubkey" ] || { rm -f "$output"; echo "community identity pubkey mismatch: $pubkey" >&2; exit 65; }
  chown root:root "$output"
  chmod 0400 "$output"
done
