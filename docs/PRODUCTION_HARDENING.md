# Production hardening

Milestone 5 adds production host controls around the lifecycle implementation. It
still assumes a single trusted Linux host and does not claim protection from a
fully compromised root account.

## Community identity custody

Clean installs do not create or require a global Buzz owner identity. Each `buzz-server communities join` operation accepts the identity for that community through a hidden terminal prompt or `--secret-file FILE`. The root CLI derives the pubkey, stores a canonical private key under `/var/lib/buzz-server/community-identities/<pubkey>.secret` with owner-only permissions, and sends only the pubkey through the lifecycle API. Multiple communities using the same pubkey share one custodied secret. Deleting the last community reference removes that secret.

The daemon uses the associated community identity for Desktop-compatible NIP-43 join verification, channel administration, and NIP-OA authorization of hosted agents. There is no public active/current identity concept.

Upgraded installations may still contain the former `/etc/buzz-server/owner-secret*` credential and `owner_secret_file` configuration. It is retained only as a migration/compatibility fallback for community records created before per-community identity association; new communities never depend on it.

## Encrypted backup and restore

A backup stops the daemon for a consistent SQLite/filesystem snapshot and archives `/etc/buzz-server`, `/var/lib/buzz-server`, and `/var/log/buzz-server` with numeric ownership. AWS KMS is used when a key ID is supplied; otherwise the archive uses a passphrase-derived scrypt key with AES-256-GCM authenticated encryption.

For a portable passphrase backup:

```sh
sudo install -m 0400 /dev/stdin /root/buzz-backup-passphrase
sudo env BUZZ_BACKUP_PASSPHRASE_FILE=/root/buzz-backup-passphrase \
  buzz-server backup /secure/buzz-backup.json
```

For KMS-backed encryption:

```sh
sudo buzz-server backup /secure/buzz-backup.json alias/buzz-server-backup
```

When the owner is held in Secret Service, backup materializes it before shutdown and embeds a decrypt-verified NIP-49 `ncryptsec` recovery artifact. Restore imports that artifact through the normal keyring-first, restricted-file-fallback custody path; it never attempts to copy OS keyring internals.

Restore validates authenticated encryption, archive paths and file types, the manifest, and the configuration digest before replacing state. It automatically restores the pre-restore configuration and state if health checks fail:

```sh
sudo env BUZZ_BACKUP_PASSPHRASE_FILE=/root/buzz-backup-passphrase \
  buzz-server restore /secure/buzz-backup.json
```

Copy encrypted backups off-host and apply an independent retention policy. KMS policy and backup storage policy remain separate controls when KMS is selected.

## Owner rotation and reauthorization

```sh
sudo buzz-server rotate-owner ./new-owner-secret
# Or keep/use KMS custody:
sudo buzz-server rotate-owner ./new-owner-secret alias/buzz-server-owner
```

Rotation writes through the selected custody backend, restarts the daemon, and restores the previous owner and custody mode if readiness fails. Startup reissues agent authorization through the
constrained signer. Confirm relay reachability and expected signed presence after
rotation; already published authorization revocation remains relay/owner policy.

## Disaster-recovery exercise

The exercise performs a real encrypted backup, rotates to a different owner key,
checks readiness, restores the pre-rotation backup, verifies the original owner
fingerprint, and runs the monitoring check. It is intentionally destructive and
requires an explicit acknowledgement:

```sh
sudo env \
  BUZZ_CONFIRM_DESTRUCTIVE_DR_EXERCISE=YES \
  BUZZ_BACKUP_PASSPHRASE_FILE=/root/buzz-backup-passphrase \
  /opt/buzz-server/current/share/deploy/disaster-recovery-exercise.sh \
  ./new-owner-secret /secure/dr-exercise.json
```

Record the output, relay presence, recovery time, and KMS audit events when KMS is used as the
Milestone 5 acceptance artifact. Run it first against a disposable community.

## Resource and network restrictions

The systemd unit applies a strict filesystem view, private devices and temporary
storage, kernel/control-group protections, hidden `/proc`, namespace and SUID
restrictions, a native system-call architecture, a 512-task limit, an 8192 file
descriptor limit, and a 4 GiB memory ceiling. It permits only Unix, IPv4, and IPv6
address families.

Relay and model endpoints vary by installation, so destination filtering belongs
in the host firewall or VPC security policy. Allow only the configured relay,
model APIs, DNS, KMS, and release endpoints; deny inbound access except the
explicit TLS lifecycle listener when enabled.

## Artifact provenance and rollback

The release workflow publishes GitHub build-provenance attestations for both
architectures. Installation requires GitHub CLI attestation verification in
addition to the release SHA-256 and strict archive manifest checks. Releases are
immutable directories, upgrades switch one symlink atomically, and failed health
checks restore the previous release.

## Monitoring and alerts

`buzz-server-healthcheck.timer` runs every minute. It checks the service, readiness
marker, lifecycle socket, and database, writes Prometheus textfile metrics to
`/var/lib/buzz-server/metrics.prom`, and logs failures to syslog. Set
`BUZZ_ALERT_COMMAND` in the health-check service environment to invoke an external
pager; the command receives `BUZZ_ALERT_REASON`.

```sh
sudo buzz-server check
systemctl status buzz-server-healthcheck.timer
cat /var/lib/buzz-server/metrics.prom
```

## Production checklist

- KMS key has rotation enabled and a least-privilege decrypt policy.
- The host uses an instance role rather than static AWS credentials.
- Plaintext owner-key files have been removed.
- Encrypted backups are copied off-host and restored in an exercise.
- Owner rotation and relay reauthorization have been exercised.
- Retention expiry and immediate purge have both been exercised.
- Host firewall/VPC egress and ingress policies are reviewed.
- Release provenance verification and rollback are exercised.
- Health metrics are collected and alert delivery is tested.
- Runtime additions use catalog entries and readiness fixtures only.
