# Production hardening

Milestone 5 adds production host controls around the lifecycle implementation. It
still assumes a single trusted Linux host and does not claim protection from a
fully compromised root account.

## Tiered owner custody

Buzz Server follows Buzz Desktop's identity-storage ordering where practical.
If `BUZZ_KMS_KEY_ID` or `BUZZ_OWNER_ENVELOPE_FILE` is configured, AWS KMS
envelope custody takes precedence. `buzz-secretsctl` encrypts the owner secret
with an AES-256-GCM data key generated and wrapped by KMS; startup decrypts it
only into `/run/buzz-server/credentials/owner-secret`.

Without KMS configuration, installation tries the Linux Secret Service keyring
using service `buzz-server` and entry `owner-identity`. It performs a direct
read-back verification before treating the write as durable. If the keyring is
unavailable, it atomically stores the same owner secret in
`/etc/buzz-server/owner-secret`, owned by root with mode `0400`, and verifies the
file contents after writing.

A successful keyring-only write creates `/etc/buzz-server/owner-secret.keyring`.
If that marker exists but the keyring entry later cannot be loaded and no
fallback file exists, startup fails closed rather than silently rotating the
owner identity. This mirrors Buzz Desktop's lost/locked identity behavior.

Desktop's implementation is currently a private Tauri module rather than an
exported Buzz crate. Buzz Server therefore uses the same `keyring` crate and
Linux backend, persistence ordering, read-back verification, and recovery
invariants. A future upstream shared secret-store crate could replace this local
implementation with literal code reuse.

For a KMS-backed import:

```sh
buzz-secretsctl encrypt \
  --kms-key-id alias/buzz-server-owner \
  --input ./owner-secret \
  --output ./owner-secret.envelope.json
```

For normal installation, provide `BUZZ_OWNER_SECRET_FILE`; add
`BUZZ_KMS_KEY_ID` only when KMS custody is desired.

## Encrypted backup and restore

A backup stops the daemon for a consistent SQLite/filesystem snapshot, archives
configuration, state, identities, workspaces, and logs with numeric ownership,
and encrypts the archive with a fresh KMS data key:

```sh
sudo buzz-serverctl backup alias/buzz-server-backup /secure/buzz-backup.envelope.json
```

Restore validates the authenticated envelope, archive member allowlist, file
types, manifest, and configuration digest before replacing state. It starts the
daemon and automatically restores the pre-restore state if health fails:

```sh
sudo buzz-serverctl restore /secure/buzz-backup.envelope.json
```

Copy encrypted backups off-host and apply an independent retention policy. The
KMS key policy and backup storage policy are separate controls.

## Owner rotation and reauthorization

```sh
sudo buzz-serverctl rotate-owner ./new-owner-secret
# Or keep/use KMS custody:
sudo buzz-serverctl rotate-owner ./new-owner-secret alias/buzz-server-owner
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
sudo env BUZZ_CONFIRM_DESTRUCTIVE_DR_EXERCISE=YES \
  /opt/buzz-server/current/share/deploy/disaster-recovery-exercise.sh \
  alias/buzz-server-backup ./new-owner-secret /secure/dr-exercise.envelope.json
```

Record the output, KMS audit events, relay presence, and recovery time as the
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
sudo buzz-serverctl check
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
