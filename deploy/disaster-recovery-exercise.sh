#!/bin/sh
set -eu

[ "$#" -eq 3 ] || { echo "usage: disaster-recovery-exercise.sh KMS_KEY_ID NEW_OWNER_SECRET_FILE BACKUP_OUTPUT" >&2; exit 64; }
[ "${BUZZ_CONFIRM_DESTRUCTIVE_DR_EXERCISE:-}" = YES ] || {
  echo "set BUZZ_CONFIRM_DESTRUCTIVE_DR_EXERCISE=YES; this rotates the owner key and restores the pre-rotation backup" >&2
  exit 64
}
kms=$1
new_owner=$2
backup=$3
controller=/usr/local/sbin/buzz-serverctl

$controller health
$controller backup "$kms" "$backup"
pre=$(/usr/local/sbin/buzz-secretsctl fingerprint --input /run/buzz-server/credentials/owner-secret)
$controller rotate-owner "$kms" "$new_owner"
$controller health
post=$(/usr/local/sbin/buzz-secretsctl fingerprint --input /run/buzz-server/credentials/owner-secret)
[ "$pre" != "$post" ] || { echo "owner rotation did not change the key fingerprint" >&2; exit 1; }
$controller restore "$backup"
$controller health
restored=$(/usr/local/sbin/buzz-secretsctl fingerprint --input /run/buzz-server/credentials/owner-secret)
[ "$pre" = "$restored" ] || { echo "restored owner fingerprint does not match the backup" >&2; exit 1; }
$controller check
cat <<EOF
production hardening exercise passed
backup=$backup
pre_rotation_owner=$pre
rotated_owner=$post
restored_owner=$restored
EOF
