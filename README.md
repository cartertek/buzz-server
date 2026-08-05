# Buzz Server

Buzz Server is a headless Buzz client for running always-on Buzz agents on a
Linux server. It manages agent identities, owner authorization, runtime launch,
reconciliation, lifecycle operations, and relay readiness without requiring
Buzz Desktop or a user laptop to remain online.

Buzz Server is not a relay and does not replace `buzz-acp`. The Buzz relay
remains the shared Nostr transport and event authority. `buzz-acp` remains the
bridge between Buzz events and ACP-compatible agent runtimes.

> **Project status:** Milestones 1–4 are implemented. The software is usable for
> controlled deployments, but production-hardening work such as encrypted owner
> custody, backup/restore exercises, monitoring, and broader resource isolation
> remains planned. See [Project planning](docs/MVP_PLAN.md).

## Features

- Durable SQLite-backed agent, operation, audit, and idempotency state
- Server-native agent key generation and constrained NIP-OA authorization
- Restart-safe supervision and adoption of `buzz-acp` and ACP runtime processes
- Version-pinned runtimes installed from digest-verified immutable packages
- Multiple isolated Buzz communities and relay connections
- Authenticated lifecycle API over a Unix socket or TLS with NIP-98
- Machine-readable `buzz-agentctl` CLI
- Create, inspect, update, enable, disable, logs, delete, purge, and operation polling
- Optional draft submission and promotion workflow
- Trusted `buzz-backend-*` provider discovery and provider compatibility testing
- Release artifacts for x86-64 and ARM64 Linux with a glibc 2.34 baseline

## Requirements

The packaged host deployment currently targets a systemd-based Linux host with:

- x86-64 or ARM64 architecture
- glibc 2.34 or newer
- root access for installation
- `curl`, `tar`, `sha256sum`, `systemctl`, `runuser`, and standard GNU utilities
- a reachable Buzz relay
- an owner Nostr secret key
- model/runtime credentials such as an OpenAI API key
- HTTPS URLs and SHA-256 values for the pinned Sprig and Codex ACP runtime packages

The release installer creates dedicated `buzz-server` and `buzz-agent` system
accounts and installs immutable releases below `/opt/buzz-server`.

## Installation

### 1. Choose a release and target

Set the release tag and host architecture:

```sh
export BUZZ_VERSION=v0.1.4
export BUZZ_TARGET=x86_64-unknown-linux-gnu
# ARM64: aarch64-unknown-linux-gnu
```

Release artifacts are published as:

```text
buzz-server-${BUZZ_VERSION}-${BUZZ_TARGET}.tar.gz
```

### 2. Download the installer from that release

```sh
curl --fail --location --proto '=https' --tlsv1.2 \
  --output /tmp/buzz-server.tar.gz \
  "https://github.com/cartertek/buzz-server/releases/download/${BUZZ_VERSION}/buzz-server-${BUZZ_VERSION}-${BUZZ_TARGET}.tar.gz"

curl --fail --location --proto '=https' --tlsv1.2 \
  --output /tmp/buzz-server.tar.gz.sha256 \
  "https://github.com/cartertek/buzz-server/releases/download/${BUZZ_VERSION}/buzz-server-${BUZZ_VERSION}-${BUZZ_TARGET}.tar.gz.sha256"

(cd /tmp && sha256sum -c buzz-server.tar.gz.sha256)
tar -xzf /tmp/buzz-server.tar.gz -C /tmp

sudo install -D -m 0755 \
  "/tmp/buzz-server-${BUZZ_VERSION}-${BUZZ_TARGET}/deploy/install-release.sh" \
  /usr/libexec/buzz-server/install-release.sh
```

### 3. Prepare configuration and secrets

Copy the example configuration and edit every example ID, relay, public key,
path, runtime, and policy value:

```sh
cp "/tmp/buzz-server-${BUZZ_VERSION}-${BUZZ_TARGET}/config/buzz-server.dev.example.json" \
  /tmp/buzz-server-config.json
```

Create a secrets environment file containing every secret referenced by the
runtime catalog. For the included Codex example:

```sh
cat > /tmp/buzz-server-secrets.env <<'EOF_SECRETS'
BUZZ_AGENT_SECRET=<agent-nsec-or-secret-hex>
BUZZ_SECRET_OPENAI_API_KEY=<openai-api-key>
EOF_SECRETS
chmod 0600 /tmp/buzz-server-secrets.env
```

Store the owner secret in a separate root-readable file:

```sh
printf '%s\n' '<owner-nsec-or-secret-hex>' > /tmp/buzz-owner-secret
chmod 0600 /tmp/buzz-owner-secret
```

Do not place secret values in `config.json`, the systemd unit, or a release directory.

### 4. Provide the pinned runtime packages

The first installation requires digest-verified Sprig and Codex ACP package
tarballs. Each archive must have the exact package root expected by the current
release:

- `sprig-0.1.0/`
- `codex-acp-1.1.7/`

Set their download locations and SHA-256 values:

```sh
export BUZZ_HARNESS_URL='https://example.invalid/sprig-0.1.0.tar.gz'
export BUZZ_HARNESS_SHA256='<64-lowercase-hex-digest>'
export BUZZ_RUNTIME_URL='https://example.invalid/codex-acp-1.1.7.tar.gz'
export BUZZ_RUNTIME_SHA256='<64-lowercase-hex-digest>'
```

### 5. Run the installer

```sh
sudo env \
  BUZZ_CONFIG_FILE=/tmp/buzz-server-config.json \
  BUZZ_SECRETS_FILE=/tmp/buzz-server-secrets.env \
  BUZZ_OWNER_SECRET_FILE=/tmp/buzz-owner-secret \
  BUZZ_HARNESS_URL="$BUZZ_HARNESS_URL" \
  BUZZ_HARNESS_SHA256="$BUZZ_HARNESS_SHA256" \
  BUZZ_RUNTIME_URL="$BUZZ_RUNTIME_URL" \
  BUZZ_RUNTIME_SHA256="$BUZZ_RUNTIME_SHA256" \
  /usr/libexec/buzz-server/install-release.sh \
  "$BUZZ_VERSION" "$BUZZ_TARGET" cartertek/buzz-server
```

The installer verifies the release checksum and archive layout, provisions the
runtime packages, runs the ACP preflight as the isolated `buzz-agent` user,
installs the systemd service, starts it, and rolls back automatically if the new
release fails its health check.

### 6. Verify the service

```sh
sudo buzz-serverctl health
sudo buzz-serverctl status
sudo journalctl -u buzz-server.service -n 100 --no-pager
```

A healthy installation prints:

```text
healthy
```

## Initial setup

The example configuration contains one community and one initial agent. Before
installing, configure at least the following:

1. `community.id`: a stable Server-local community ID.
2. `community.display_name`: an operator-friendly name.
3. `community.relay_url`: the authoritative relay WebSocket URL.
4. `agent.id`: a stable initial agent ID.
5. `agent.community_config_id`: the same ID used by `community.id`.
6. `agent.display_name` and `agent.system_prompt`.
7. `expected_agent_pubkey`: the public key derived from `BUZZ_AGENT_SECRET`.
8. `signer_conditions`: the constrained owner authorization policy.
9. Runtime paths, versions, required secrets, and environment values.
10. Lifecycle API administrator and draft-submitter identities.

The daemon validates owner authorization, relay authentication, runtime
preflight, process identity, and expected signed presence before reporting ready.

## Configuration

The installed configuration files are:

```text
/etc/buzz-server/config.json
/etc/buzz-server/secrets.env
/etc/buzz-server/owner-secret
```

The JSON schema is available at
[`config/buzz-server.schema.json`](config/buzz-server.schema.json), and a complete
starting configuration is available at
[`config/buzz-server.dev.example.json`](config/buzz-server.dev.example.json).

### Core paths

| Field | Purpose |
|---|---|
| `state_database` | SQLite lifecycle and audit database |
| `receipt_file` | Durable process receipt for the configured initial agent |
| `signer_socket` | Constrained owner-signer IPC socket |
| `log_directory` | Redacted managed-agent logs |
| `working_directory` | Server working root |
| `workspace_path` | Initial agent workspace |
| `runtime_path` | Initial agent runtime state |

Production configuration paths are restricted to the `/var/lib/buzz-server`,
`/var/log/buzz-server`, and `/run/buzz-server` roots defined by the schema.

### Community and agent

`community` defines the authoritative relay URL and Server-local community
identity. `agent` defines the initially managed agent, including its display
name, system prompt, runtime selection, environment, and desired state.
Additional agents are created through the lifecycle API or `buzz-agentctl`.

### Runtime catalog

`runtime_catalog.runtimes` maps runtime IDs to exact versions, executable paths,
arguments, preflight probes, and required secret references. Runtime package
integrity is enforced during installation; secret values remain outside the
catalog and are resolved from `secrets.env`.

### Lifecycle API

`lifecycle_api` configures:

- Unix socket path
- administrator UIDs
- draft-submitter UIDs
- recoverable-delete retention period
- optional TLS listener and NIP-98 public-key allowlists

The Unix socket authenticates callers using kernel peer credentials. The remote
listener uses server-side TLS plus NIP-98 client authentication; it is not mTLS.
See [Lifecycle API](docs/LIFECYCLE_API.md) for the complete wire and authorization contract.

## Using the CLI

The installer places `buzz-agentctl` in `/usr/local/bin`. Responses are compact
JSON on stdout.

```sh
sudo buzz-agentctl list
sudo buzz-agentctl get --agent agent_...
sudo buzz-agentctl logs --agent agent_... --limit 100
sudo buzz-agentctl disable \
  --agent agent_... \
  --idempotency maintenance-1 \
  --correlation maintenance-1
sudo buzz-agentctl operation --operation operation_...
```

See [`docs/CLI.md`](docs/CLI.md) for every command and option.

## Updating

Install a newer immutable release by selecting its tag and target:

```sh
export NEW_BUZZ_VERSION=v0.1.4
export BUZZ_TARGET=x86_64-unknown-linux-gnu
sudo buzz-serverctl redeploy "$NEW_BUZZ_VERSION" "$BUZZ_TARGET" cartertek/buzz-server
```

Set `NEW_BUZZ_VERSION` to the release being installed. For ARM64, use
`aarch64-unknown-linux-gnu`.

The updater:

- downloads and verifies the tagged release;
- refuses to overwrite an existing immutable release directory;
- preserves `/etc/buzz-server` configuration and secrets;
- preserves state, workspaces, runtime state, and logs;
- revalidates the installed runtime packages;
- runs the isolated runtime preflight;
- atomically changes `/opt/buzz-server/current`;
- restarts the daemon and waits for health;
- restores the previous release automatically if health fails.

Review configuration and schema changes before upgrading. The installer does not
overwrite existing configuration or secret files.

## Rollback

List installed releases:

```sh
ls -1 /opt/buzz-server/releases
```

Select an earlier installed release by its directory name:

```sh
sudo buzz-serverctl rollback v0.1.4-x86_64-unknown-linux-gnu
```

Rollback preserves the database, identities, runtime state, workspaces, and
configuration. It restores the previous release automatically if the selected
rollback target fails health checks.

## Service management

```sh
sudo buzz-serverctl health
sudo buzz-serverctl status
sudo buzz-serverctl restart
sudo buzz-serverctl stop
sudo buzz-serverctl start
```

System logs are available through:

```sh
sudo journalctl -u buzz-server.service -f
```

## Building from source

The project requires Rust 1.88.0 and native build tools.

```sh
git clone https://github.com/cartertek/buzz-server.git
cd buzz-server
rustup toolchain install 1.88.0 --profile minimal --component rustfmt,clippy
rustup override set 1.88.0
cargo test --all-targets --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo build --release --locked --bins
```

The supported installation contract is the tagged release package and installer,
not copying a locally built binary directly into the production filesystem.

## Security model

- The owner key is loaded through a root-only systemd credential file.
- Runtime and model credentials are stored separately from JSON configuration.
- Agent processes run under the restricted `buzz-agent` account.
- Administrative Unix access is authorized using `SO_PEERCRED` UIDs.
- Remote API access uses TLS and request-bound NIP-98 signatures with replay protection.
- Provider binaries require explicit path and digest trust before staged execution.
- Audit and log records reject credential-shaped material.

See [Security](docs/SECURITY.md) and [Architecture](docs/ARCHITECTURE.md).

## Documentation

- [Installation and host deployment details](deploy/README.md)
- [Configuration schema](config/buzz-server.schema.json)
- [Lifecycle API](docs/LIFECYCLE_API.md)
- [`buzz-agentctl` CLI](docs/CLI.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Security](docs/SECURITY.md)
- [Provider compatibility](docs/PROVIDER_COMPATIBILITY.md)
- [Compatibility with Buzz](docs/COMPATIBILITY_WITH_BUZZ.md)
- [Project planning and status](docs/MVP_PLAN.md)
- [Implementation milestones](docs/FOLLOW_UP_IMPLEMENTATION_PLAN.md)

## Contributing

Pull requests run formatting, tests, strict Clippy, pinned upstream provider
compatibility checks, and x86-64 and ARM64 release builds. Keep changes scoped,
include tests for behavioral changes, and update the relevant documentation and
configuration schema together.

## License

Licensed under the Apache License 2.0. See [`LICENSE`](LICENSE).
