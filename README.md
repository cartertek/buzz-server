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
- Unified `buzz-server` CLI
- Create, inspect, update, enable, disable, logs, delete, purge, and operation polling
- Optional draft submission and promotion workflow
- Trusted `buzz-backend-*` provider discovery and provider compatibility testing
- Release artifacts for x86-64 and ARM64 Linux with a glibc 2.34 baseline

## Requirements

The packaged host deployment currently targets a systemd-based Linux host with:

- x86-64 or ARM64 architecture
- glibc 2.34 or newer
- root access for installation
- `curl`, `tar`, `sha256sum`, `systemctl`, `runuser`, AWS CLI, and standard GNU utilities
- a reachable Buzz relay
- an owner Nostr secret key; AWS KMS is optional and takes precedence when configured
- model/runtime credentials such as an OpenAI API key
- HTTPS URLs and SHA-256 values for the pinned Sprig and Codex ACP runtime packages

The release installer creates dedicated `buzz-server` and `buzz-agent` system
accounts and installs immutable releases below `/opt/buzz-server`.

## Installation

Download the latest release for your architecture:

```sh
curl -LO https://github.com/cartertek/buzz-server/releases/latest/download/buzz-server-x86_64-unknown-linux-gnu.tar.gz
```

For ARM64, replace `x86_64-unknown-linux-gnu` with `aarch64-unknown-linux-gnu`.

To install a specific version:

```sh
curl -LO https://github.com/cartertek/buzz-server/releases/download/v0.1.4/buzz-server-x86_64-unknown-linux-gnu.tar.gz
```

Then extract it and run the bundled installer:

```sh
tar -xzf buzz-server-x86_64-unknown-linux-gnu.tar.gz
sudo ./buzz-server/deploy/install.sh
```

On first install, the installer asks for the required values and creates the Buzz
Server configuration and secret files. The installer is part of the downloaded
archive and installs that exact build.

The same process is used for updates. Running the installer from a newer or older
release atomically installs that release over the existing installation while
preserving configuration, secrets, state, workspaces, and logs. If the new
release fails its health check, the previous release is restored automatically.

For unattended installation, pass the prompted values as environment variables
and add `--non-interactive`. See [host deployment details](deploy/README.md) for
the variable names and lower-level deployment contract.

## Configuration

Configuration lives in `/etc/buzz-server`. Edit `config.json` for server and
agent settings; runtime secrets remain in `secrets.env`. The owner key is stored
through KMS when configured, otherwise through the OS keyring or restricted-file
fallback. The schema is [`config/buzz-server.schema.json`](config/buzz-server.schema.json).

## Using the CLI

The installer places one operator command, `buzz-server`, in `/usr/local/bin`. Service operations are top-level commands; agent lifecycle operations use `buzz-server agent`, and key management uses `buzz-server secrets`.

```sh
sudo buzz-server agent list
sudo buzz-server agent get --agent agent_...
sudo buzz-server agent logs --agent agent_... --limit 100
sudo buzz-server agent disable \
  --agent agent_... \
  --idempotency maintenance-1 \
  --correlation maintenance-1
sudo buzz-server agent operation --operation operation_...
```

See [`docs/CLI.md`](docs/CLI.md) for every command and option.

## Rollback

List installed releases:

```sh
ls -1 /opt/buzz-server/releases
```

Select an earlier installed release by its directory name:

```sh
sudo buzz-server rollback v0.1.4-x86_64-unknown-linux-gnu
```

Rollback preserves the database, identities, runtime state, workspaces, and
configuration. It restores the previous release automatically if the selected
rollback target fails health checks.

## Service management

```sh
sudo buzz-server health
sudo buzz-server status
sudo buzz-server restart
sudo buzz-server stop
sudo buzz-server start
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

- The owner key is stored through AWS KMS when configured, otherwise in the OS secret manager with a restricted key-file fallback, and materialized only into a root-only runtime file.
- Runtime and model credentials are stored separately from JSON configuration.
- Agent processes run under the restricted `buzz-agent` account.
- Administrative Unix access is authorized using `SO_PEERCRED` UIDs.
- Remote API access uses TLS and request-bound NIP-98 signatures with replay protection.
- Provider binaries require explicit path and digest trust before staged execution.
- Audit and log records reject credential-shaped material.

See [Security](docs/SECURITY.md), [Production hardening](docs/PRODUCTION_HARDENING.md), and [Architecture](docs/ARCHITECTURE.md).

## Documentation

- [Installation and host deployment details](deploy/README.md)
- [Configuration schema](config/buzz-server.schema.json)
- [Lifecycle API](docs/LIFECYCLE_API.md)
- [`buzz-server` CLI](docs/CLI.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Security](docs/SECURITY.md)
- [Production hardening](docs/PRODUCTION_HARDENING.md)
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
