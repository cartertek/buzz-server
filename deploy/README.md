# Buzz Server host deployment

Releases also install `buzz-server agents`, a machine-readable lifecycle client for
the authenticated Unix socket. See [`docs/CLI.md`](../docs/CLI.md) for the full
command reference and [`docs/LIFECYCLE_API.md`](../docs/LIFECYCLE_API.md) for the
wire and authorization contracts. For example:

```sh
buzz-server agents list
buzz-server agents get --agent agent_...
buzz-server agents disable --agent agent_...
buzz-server agents reload --agent agent_...
buzz-server agents operation --operation operation_...
```

Create, update, enable, disable, reload, logs, recoverable delete, immediate purge, and
draft commands use the same option-shaped interface. Product responses are
emitted as one compact JSON object on stdout; transport and usage failures go
to stderr.

CI gates every pull request and `master` push. Pushing an immutable `v*`
tag builds x86-64 and ARM64 Linux tarballs, verifies all build jobs, and only
then attaches the tarballs and SHA-256 files to a GitHub Release. The `current`
symlink is the only mutable host release pointer. Release tarballs are the first
deployment format; a GHCR image can be added later without changing this gate.
Each release is assembled in a same-filesystem staging directory, made
root-owned and non-writable, and renamed into place atomically. Running the
installer from a newer or older release switches to that release while preserving
configuration, secrets, state, workspaces, and logs. If that immutable release is
already present, the installer verifies that its contents exactly match the supplied
package and reuses it rather than overwriting it.
Protect `master` with the CI check, restrict creation/deletion of `v*`
tags, and grant the release workflow `contents:write` only. Release reruns refuse
to replace assets on an existing GitHub Release.

```sh
sudo install -D -m 0755 deploy/install-release.sh /usr/libexec/buzz-server/install-release.sh
sudo install -D -m 0755 deploy/buzz-server /usr/local/bin/buzz-server
sudo install -D -m 0644 deploy/buzz-server.service /etc/systemd/system/buzz-server.service
sudo install -d -o root -g buzz-server -m 0750 /etc/buzz-server
sudo BUZZ_CONFIG_FILE=/path/to/config.json \
  BUZZ_SECRETS_FILE=/path/to/secrets.env \
  /usr/libexec/buzz-server/install-release.sh v1.0.0 x86_64-unknown-linux-gnu OWNER/REPOSITORY
sudo buzz-server health
```

For a viable unattended first install, set `BUZZ_CONFIG_FILE` and `BUZZ_SECRETS_FILE` to operator-prepared files; neither is overwritten on later deployments. Community Nostr identities are supplied later through `buzz-server communities join`. The first install must also provision the two immutable executables referenced
by the example configuration. Either install complete packages at those exact
paths before deployment, including their `.package.tar.gz` and
`.package.sha256` records, or pass `BUZZ_HARNESS_URL`, `BUZZ_HARNESS_SHA256`,
`BUZZ_RUNTIME_URL`, and `BUZZ_RUNTIME_SHA256` to `install-release.sh`. Each URL
must be an HTTPS tarball rooted at exactly `sprig-0.1.0/` or
`codex-acp-1.1.7/`; this intentionally packages Node entrypoints together with
their required modules rather than treating a JavaScript shim as a standalone
binary. The `bin/` entrypoints must be materialized regular executable files
(not package-manager symlinks). Archives are checksum-verified, constrained to regular files and
directories, extracted into same-filesystem staging directories, then renamed
atomically into root-owned, `buzz-agent`-group-readable immutable locations. Every
deployment rechecks the retained archive digest and entrypoint.
Before changing the live release pointer, the installer runs the exact
`buzz-runtime-probe codex-acp-version` availability/version preflight, copied from Buzz Desktop's bounded `codex-acp --version` probe, as the `buzz-server` control-plane account. This catches missing
Node, modules, loaders, and execute/read permissions.

## Agent Unix accounts

Buzz Server creates a shared `buzz-agent` system account with `buzz-agent` as
its primary group. By default, every managed agent process runs under the account
selected by `default_agent.filesystem.user` in `/etc/buzz-server/config.json`,
which defaults to `buzz-agent`, while each agent retains an isolated workspace and
runtime directory.
Reconciliation owns those directories as the selected account and the
`buzz-agent` group with mode `0770`, including upgrades from earlier builds that
used generated per-agent accounts. Legacy generated accounts are not removed
automatically.

An operator can override that default by setting `filesystem.user` in the agent
JSON or passing `--filesystem-user USER` to `agents create` or `agents update`. The account must
already exist and already belong to the `buzz-agent` group so it can execute
the packaged runtimes. Buzz Server does not alter or remove explicitly selected
accounts; the operator remains responsible for their groups, modes, and ACLs.
Buzz Server does chown that agent's own workspace and runtime directories to the
selected account with `buzz-agent` as their group and mode `0770`.

`secrets.env` supplies runtime API secrets referenced by configuration. Hosted-agent Nostr identities are generated and held in the server's root-only identity custody. Community identities supplied during `communities join` are separately custodied by pubkey under `/var/lib/buzz-server/community-identities`; no community private key belongs in `config.json`, the lifecycle API, the service unit, or a release directory.

Updates and rollbacks use the same installer procedure: extract the desired newer
or older release artifact and run its `deploy/install.sh`. The release pointer is
switched atomically; if the selected release fails its health check, the previously
active release is restored automatically.

The installer enables the service for boot and restarts it after switching the
release pointer. The systemd service uses `KillMode=control-group`; the daemon
reconciles every hosted agent from durable database state after startup.

Before a live deployment, replace example artifact paths, versions, and checksums.
Install the pinned Sprig/Buzz harness and ACP runtime artifacts at those immutable
paths. After the service is healthy, add communities and hosted agents through
`buzz-server communities` and `buzz-server agents`. Validate relay reachability,
NIP-42 authentication, NIP-OA authorization, model credentials, writable state
paths, and the harness preflight command in the service account's restricted
context.
