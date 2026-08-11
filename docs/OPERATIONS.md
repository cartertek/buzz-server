# Production operations

This guide covers operational controls shipped with Buzz Server:
community identity custody, encrypted backup and restore, host restrictions, release verification, and monitoring.

## Community identity custody

Clean installs do not create or require a global Buzz owner identity. Each `buzz-server communities join` operation accepts the identity for that community through a hidden terminal prompt or `--secret-file FILE`. The root CLI derives the pubkey and stores the private key per pubkey. When `identity_custody.kms_key_id` is configured, the persisted form is a KMS envelope. Otherwise Buzz Server follows Buzz Desktop: it prefers the OS keyring and falls back to an owner-only local file when no keyring backend is available. The daemon reads only root-only ephemeral materializations under `/run/buzz-server/community-identities`. Only the pubkey crosses the lifecycle API. Multiple communities using the same pubkey share one custodied secret, and deleting the last reference removes its custody artifacts.

The daemon uses the associated community identity for Desktop-compatible NIP-43 join verification, channel administration, and NIP-OA authorization of hosted agents. There is no public active/current identity concept.

When upgrading from the former single-owner configuration, the installer migrates that identity into per-community custody and updates legacy community records before starting the new daemon. The old global owner configuration and credential are removed only after the upgraded service passes health checks.

## Encrypted backup and restore

A backup stops the daemon for a consistent SQLite/filesystem snapshot and archives `/etc/buzz-server`, `/var/lib/buzz-server`, and `/var/log/buzz-server` with numeric ownership. KMS envelopes and restricted-file identity custody are included directly. OS-keyring identities are exported as verified NIP-49 recovery artifacts inside the encrypted backup and restored through the normal keyring-first fallback path. AWS KMS is used when a key ID is supplied; otherwise the archive uses a passphrase-derived scrypt key with AES-256-GCM authenticated encryption.

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

Restore validates authenticated encryption, archive paths and file types, the manifest, and the configuration digest before replacing state. It automatically restores the pre-restore configuration and state if health checks fail:

```sh
sudo env BUZZ_BACKUP_PASSPHRASE_FILE=/root/buzz-backup-passphrase \
  buzz-server restore /secure/buzz-backup.json
```

Copy encrypted backups off-host and apply an independent retention policy. KMS policy and backup storage policy remain separate controls when KMS is selected.

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
- Community identity files are root-only and included in encrypted backups.
- Encrypted backups are copied off-host and restored in an exercise.
- Retention expiry and immediate purge have both been exercised.
- Host firewall/VPC egress and ingress policies are reviewed.
- Release provenance verification and rollback are exercised.
- Health metrics are collected and alert delivery is tested.
- Runtime additions use catalog entries and readiness fixtures only.
