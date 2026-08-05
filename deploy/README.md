# Buzz Server host deployment

Releases also install `buzz-agentctl`, a machine-readable lifecycle client for
the owner-only Unix socket. For example:

```sh
buzz-agentctl list
buzz-agentctl get --agent agent_...
buzz-agentctl disable --agent agent_... --idempotency disable-1 --correlation maintenance-1
buzz-agentctl operation --operation operation_...
```

Create, update, enable, disable, logs, recoverable delete, immediate purge, and
draft commands use the same option-shaped interface. Product responses are
emitted as one compact JSON object on stdout; transport and usage failures go
to stderr.

CI gates every pull request and `master` push. Pushing an immutable `v*`
tag builds x86-64 and ARM64 Linux tarballs, verifies all build jobs, and only
then attaches the tarballs and SHA-256 files to a GitHub Release. The `current`
symlink is the only mutable host release pointer. Release tarballs are the first
deployment format; a GHCR image can be added later without changing this gate.
Each release is assembled in a same-filesystem staging directory, made
root-owned and non-writable, and renamed into place atomically. Reinstalling an
existing version is rejected rather than mutating its directory.
Protect `master` with the CI check, restrict creation/deletion of `v*`
tags, and grant the release workflow `contents:write` only. Release reruns refuse
to replace assets on an existing GitHub Release.

```sh
sudo install -D -m 0755 deploy/install-release.sh /usr/libexec/buzz-server/install-release.sh
sudo install -D -m 0755 deploy/buzz-serverctl /usr/local/sbin/buzz-serverctl
sudo install -D -m 0644 deploy/buzz-server.service /etc/systemd/system/buzz-server.service
sudo install -d -o root -g buzz-server -m 0750 /etc/buzz-server
sudo install -o root -g buzz-server -m 0640 config/buzz-server.dev.example.json /etc/buzz-server/config.json
sudo install -o root -g buzz-server -m 0640 /dev/null /etc/buzz-server/secrets.env
sudo /usr/libexec/buzz-server/install-release.sh v1.0.0 x86_64-unknown-linux-gnu OWNER/REPOSITORY
sudo buzz-serverctl health
```

For a viable unattended first install, set `BUZZ_CONFIG_FILE`,
`BUZZ_SECRETS_FILE`, and `BUZZ_OWNER_SECRET_FILE` to operator-prepared files;
the owner secret is installed root-only for systemd `LoadCredential`, and none is overwritten on later
deployments. The first install must also provision the two immutable executables referenced
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
atomically into root-owned, `buzz-agent`-readable immutable locations. Every
deployment rechecks the retained archive digest and entrypoint.
Before changing the live release pointer, the installer runs the exact
`buzz-acp models --json --agent-command ... --agent-args acp` preflight as the
isolated `buzz-agent` account with a minimal environment. This catches missing
Node, modules, loaders, and execute/read permissions.

`secrets.env` supplies the environment names referenced by configuration. It
must contain stable, distinct owner and agent secrets plus runtime API secrets.
The constrained signer derives and verifies the NIP-OA tag at every start;
no secret belongs in `config.json`, the service unit, or a release directory.

Rollback retains state and selects a previously installed immutable binary:

```sh
sudo buzz-serverctl rollback v1.0.0-x86_64-unknown-linux-gnu
```

The installer enables the service for boot and restarts it after switching the
release pointer. The unit uses `KillMode=process` so a planned service restart
stops Buzz Server itself while leaving its receipt-bound agent child available
for validation and adoption by the replacement daemon. Unexpected relay or
signer failure remains fail-closed in the daemon and terminates the child.

Before a live deployment, replace all example IDs, relay, public key, artifact
paths, versions, and checksums. Install the pinned Sprig/Buzz harness and ACP
runtime artifacts at those immutable paths. Validate relay reachability, NIP-42
authentication, NIP-OA authorization, model credentials, writable state paths,
and the harness preflight command in the service account's restricted context.
