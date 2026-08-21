# Buzz Server

Buzz Server is a headless Buzz client for running always-on Buzz agents on a
Linux server. It manages agent identities, owner authorization, runtime launch,
reconciliation, lifecycle operations, and relay readiness without requiring
Buzz Desktop or a user laptop to remain online.

Buzz Server is not a relay and does not replace `buzz-acp`. The Buzz relay
remains the shared Nostr transport and event authority. `buzz-acp` remains the
bridge between Buzz events and ACP-compatible agent runtimes.

> **Project status:** Buzz Server provides the durable local agent lifecycle,
> authenticated administration, provider-protocol compatibility, release packaging,
> and production host hardening described below. External providers are compatible
> deployment targets, but selecting one as an agent's durable lifecycle backend is
> not yet implemented.

## Features

- Durable SQLite-backed agent, operation, audit, and idempotency state
- Server-native agent key generation and constrained NIP-OA authorization
- Restart-safe supervision and adoption of `buzz-acp` and ACP runtime processes
- Version-pinned runtimes installed from digest-verified immutable packages
- Multiple isolated Buzz communities and relay connections
- Authenticated lifecycle API over a Unix socket or TLS with NIP-98
- Command-line management for server operations, agents, and secrets
- Create, inspect, update, enable, disable, logs, delete, purge, and operation inspection
- Optional draft submission and promotion workflow
- Trusted `buzz-backend-*` provider discovery and provider compatibility testing
- Release artifacts for x86-64 and ARM64 Linux with a glibc 2.34 baseline

## Requirements

The packaged host deployment currently targets a systemd-based Linux host with:

- x86-64 or ARM64 architecture
- glibc 2.34 or newer
- root access for installation
- `curl` and `tar`
- AWS CLI when using AWS KMS for identity custody or backups
- a reachable Buzz relay
- model/runtime credentials such as an OpenAI API key
- HTTPS URLs and SHA-256 values for the pinned Sprig and Codex ACP runtime packages

The release installer creates the control-plane `buzz-server` account and common
`buzz-agent` group, and installs immutable releases below `/opt/buzz-server`.

## Installation

Download the latest [release](https://github.com/cartertek/buzz-server/releases) for your architecture:

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

The same process is used for updates and rollbacks. Running the installer from a
newer or older release installs that release over the existing installation while
preserving configuration, secrets, state, workspaces, and logs.

For unattended installation, pass the prompted values as environment variables
and add `--non-interactive`. See [host deployment details](deploy/README.md) for
the variable names and lower-level deployment contract.

## Getting started

Buzz Server keeps community and agent lifecycle state in its durable database and keeps
human-editable agent configuration in files. A clean installation starts with no
communities or agents. Join an existing community using the Nostr identity that this
server should use for that community:

```sh
sudo buzz-server communities join \
  --display-name 'Engineering' \
  --relay-url 'wss://relay.example.com/'
# The CLI prompts for the Nostr private key without echoing it.
```

Optionally, create a reusable persona for creating new agents:

```sh
sudo buzz-server personas create \
  --display-name 'Software engineer' \
  --system-prompt 'Help with software engineering work in this channel.' \
  --runtime codex-acp
```

Create an agent in the target community using the returned `community_...` ID:

```sh
sudo buzz-server agents create \
  --community community_... \
  --display-name 'Build agent' \
  --runtime codex-acp
```

Optionally pass either `--persona <persona-id>` or `--system-prompt '...'`.

For a prompt maintained outside the JSON config, use
`--system-prompt-file /absolute/path/to/prompt.md`. The path must be absolute
and readable by the Buzz Server service account. A non-empty inline
`system_prompt` takes precedence; otherwise the file is read as strict UTF-8
each time the agent configuration is resolved, so edits take effect on the
next restart or reconciliation. If the file is inside the agent's
workspace, workspace-write access can therefore influence the next session's
prompt; treat that as part of the workspace security boundary.

Creating the agent starts its ACP runtime against the selected community relay.
The response includes the new `agent_...` ID and the agent's Nostr public key.

Copy the public key from the create response, or get it again later with:

```sh
sudo buzz-server agents pubkey --agent agent_...
```

Add the newly created agent to a channel:

```sh
sudo buzz-server channels add-member \
  --community community_... \
  --channel <channel-uuid> \
  --pubkey <agent-public-key>
```

Buzz Server defaults an omitted add-member role to `bot`. Pass `--role`
explicitly to select a different role.

Once the agent is a channel member, its ACP runtime discovers the membership and
subscribes to mentions automatically.

Use `buzz-server agents list`, `buzz-server agents get --agent agent_...`, and
`buzz-server agents logs --agent agent_...` to inspect the hosted agent. See
[`docs/CLI.md`](docs/CLI.md) for the complete Buzz Server command reference.

## Configuration

Persistent host/runtime configuration lives in `/etc/buzz-server/config.json`; runtime
secrets remain in `secrets.env`. Human-editable agent configuration lives in
`/var/lib/buzz-server/agent-config/agents/*.json`, and optional personas live in
`/var/lib/buzz-server/agent-config/personas/*.json`. Buzz Server reloads those files
when the service starts. Lifecycle/process state, community records, operations, and
other machine-managed state remain in SQLite.

Personas can be created and inspected with `buzz-server personas create`, `get`,
`list`, `update`, and `delete`, or edited directly as JSON. Pass a persona ID to
`buzz-server agents create --persona ID`. Agent files may override the persona's
runtime and environment; prompt, model, and provider follow the linked persona, as
in Buzz Desktop.

Set per-agent environment variables in the agent file's `environment` object. Persona
environment variables are inherited by persona-backed agents, with agent values taking
precedence. For example, a Codex agent can use a dedicated Codex home and subscribe to
all messages in every channel it belongs to with:

```json
"environment": {
  "CODEX_HOME": "/var/lib/buzz-server/agents/agent_.../runtime/codex-home",
  "BUZZ_ACP_SUBSCRIBE": "all"
}
```

`CODEX_HOME` is consumed by Codex; `BUZZ_ACP_SUBSCRIBE` is consumed by `buzz-acp`.
The default `BUZZ_ACP_SUBSCRIBE=mentions` subscribes to events that structurally
mention the agent. Set it to `all` to subscribe to every message in every channel the
agent belongs to. This setting controls message selection only; the agent's
`respond_to` policy still controls which authors it accepts work from. Changes to an
existing agent's environment take effect after restarting Buzz Server.

Agent processes use the account in `default_agent.filesystem.user` (the shared
`buzz-agent` Unix account when omitted) unless the agent file overrides it; see
[host deployment details](deploy/README.md#agent-unix-accounts) for account and
`filesystem.user` configuration.

Subscription does not grant channel membership. Running agents automatically discover
new channel memberships and subscribe using their configured `BUZZ_ACP_SUBSCRIBE`
mode. Separately, `auto_join_open_channels` accepts
three modes:

- `"disabled"` (default) never automatically joins channels;
- `"all"` joins current and future open channels;
- `"new"` joins only open channels when they are first created.

For example, to join only future open channels, add this top-level agent setting:

```json
"auto_join_open_channels": "new"
```

Auto-join is event-driven: Buzz Server watches channel-creation events and joins the
agent identity when an eligible open channel appears. The `"new"` enablement time is
stored durably, so channels created while Buzz Server is offline are joined after it
restarts, without joining older channels. Switching away from `"new"` clears that
boundary; enabling it again starts a new boundary. Private channels remain invite-only.

Community identities are stored per public key using AWS KMS when configured; otherwise Buzz Server follows Buzz Desktop by preferring the OS keyring and falling back to an owner-only local file when no keyring backend is available. Only ephemeral materializations under `/run/buzz-server` are read by the daemon. The host configuration schema is
[`config/buzz-server.schema.json`](config/buzz-server.schema.json).


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

- Community identity keys use KMS when configured, otherwise OS-keyring-first custody with an owner-only file fallback, and are never exposed through the lifecycle API.
- Runtime and model credentials are stored separately from JSON configuration.
- Agent processes run under the existing account selected by the agent's
  `filesystem.user`, then `default_agent.filesystem.user`, and finally the shared
  `buzz-agent` Unix account.
- Administrative Unix access is authorized using `SO_PEERCRED` UIDs.
- Remote API access uses TLS and request-bound NIP-98 signatures with replay protection.
- Provider binaries require explicit path and digest trust before staged execution.
- Audit and log records reject credential-shaped material.

See [Security](docs/SECURITY.md), [Production operations](docs/OPERATIONS.md), and [Architecture](docs/ARCHITECTURE.md).

## Documentation

- [Installation and host deployment details](deploy/README.md)
- [Configuration schema](config/buzz-server.schema.json)
- [Lifecycle API](docs/LIFECYCLE_API.md)
- [`buzz-server` CLI](docs/CLI.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Security](docs/SECURITY.md)
- [Production operations](docs/OPERATIONS.md)
- [Provider compatibility](docs/PROVIDER_COMPATIBILITY.md)
- [Provider lifecycle integration](docs/PROVIDER_LIFECYCLE_INTEGRATION.md)
- [Compatibility with Buzz](docs/COMPATIBILITY_WITH_BUZZ.md)

## Contributing

Pull requests run formatting, tests, strict Clippy, pinned upstream provider
compatibility checks, and x86-64 and ARM64 release builds. Keep changes scoped,
include tests for behavioral changes, and update the relevant documentation and
configuration schema together.

## License

Licensed under the Apache License 2.0. See [`LICENSE`](LICENSE).
