#!/bin/sh
set -eu
[ "$#" -eq 2 ] || { echo "usage: disaster-recovery-exercise.sh NEW_OWNER_SECRET_FILE BACKUP_OUTPUT" >&2; exit 64; }
[ "${BUZZ_CONFIRM_DESTRUCTIVE_DR_EXERCISE:-}" = YES ] || { echo "set BUZZ_CONFIRM_DESTRUCTIVE_DR_EXERCISE=YES; this rotates the owner key and restores the pre-rotation backup" >&2; exit 64; }
new_owner=$1
backup=$2
controller=/usr/local/bin/buzz-server
$controller health
$controller backup "$backup" "${BUZZ_KMS_KEY_ID:-}"
pre=$(/usr/local/bin/buzz-server secret fingerprint --input /run/buzz-server/credentials/owner-secret)
$controller rotate-owner "$new_owner" "${BUZZ_KMS_KEY_ID:-}"
$controller health
post=$(/usr/local/bin/buzz-server secret fingerprint --input /run/buzz-server/credentials/owner-secret)
[ "$pre" != "$post" ] || { echo "owner rotation did not change the key fingerprint" >&2; exit 1; }
$controller restore "$backup"
$controller health
restored=$(/usr/local/bin/buzz-server secret fingerprint --input /run/buzz-server/credentials/owner-secret)
[ "$pre" = "$restored" ] || { echo "restored owner fingerprint does not match the backup" >&2; exit 1; }
$controller check
printf 'production hardening exercise passed\nbackup=%s\npre_rotation_owner=%s\nrotated_owner=%s\nrestored_owner=%s\n' "$backup" "$pre" "$post" "$restored"
