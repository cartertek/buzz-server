# Security and trust boundaries

## Primary boundaries

- API callers may request lifecycle operations but may not request arbitrary
  signatures or raw supervisor commands.
- The MVP local backend may request only typed launch and lifecycle operations
  from its least-privilege headless process supervisor, never arbitrary commands.
- The signer is isolated from the local backend, the API daemon, the relay,
  supervised agent processes, and workspaces as far as the host permits.
- Supervised agent processes never receive the owner private key or Server
  administrative credentials.
- Every agent belongs to exactly one configured community and receives only that
  community's relay URL and owner authorization.

## Owner signing

Server-native creation requires the owner identity because each authorization is
bound to a newly generated agent public key. Authorization construction and
verification call Buzz's shared `buzz-sdk` NIP-OA implementation so Desktop and
Server remain byte-for-byte compatible. The signer exposes only a structured,
policy-limited “authorize agent” operation—never arbitrary Nostr signing.

Production owner import uses the KMS envelope workflow documented in
[PRODUCTION_HARDENING.md](PRODUCTION_HARDENING.md). The encrypted envelope is
persistent; plaintext exists only in a root-only runtime path while the daemon is
running.

Envelope encryption with KMS protects offline ciphertext, backups, and snapshots;
provides audit records and a kill switch; and limits decrypt permission to the
signer. It does not protect against a fully compromised live host that can invoke
or inspect the authorized signer.

## Required controls

- secret redaction in API, local-backend, process-supervisor, runtime, and audit
  logs;
- no plaintext owner key, agent key, auth tag, or model credential in command
  arguments, Server-wide or ambient inherited environments, launch receipts,
  registry metadata, logs, or audit records;
- construct each supervised process environment from an explicit allowlist;
  release only the target agent's required secrets immediately before launch,
  preferably through private file descriptors or mode-restricted files, and do
  not propagate the Server process environment wholesale;
- stable agent keys across ordinary updates;
- immediate signer/KMS disable path;
- encrypted off-host backup and tested restoration;
- rate, replay, capability, expiry, and caller checks in the signer;
- memory-dump prevention and hardened service accounts;
- key versioning, rotation, revocation, and reauthorization runbooks;
- idempotent operations to prevent duplicate identity creation;
- explicit deletion retention policy; no implicit workspace destruction;
- community-scoped registry access, caches, jobs, local launch configuration,
  secrets, workspaces, process state, logs, and audit records;
- no implicit cross-community queries, defaults, or data movement;
- a least-privilege process-supervisor boundary with an explicit executable,
  argument, environment, filesystem, signal, and lifecycle policy instead of an
  arbitrary command surface in the API daemon.

## Future provider and Compose controls

- Provider executables are trusted deployment plugins because the current Buzz
  protocol may give them agent secrets. Future provider installation is
  administrator-only and requires allowlisting, checksum pinning, bounded
  input/output, redacted logging, and subprocess sandboxing.
- A future Docker Compose provider must keep plaintext secrets out of generated
  YAML, launch receipts, logs, and audit records; isolate projects and secret
  paths by community and agent; deny agents access to the Docker socket; and use
  a narrow helper rather than granting Docker access to the API daemon.

## Residual threat-model questions

- agent-to-signer network and filesystem isolation;
- authorization revocation semantics for already issued NIP-OA tags;
- identity attribution and membership policy for future external bridges.
