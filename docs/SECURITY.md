# Security and trust boundaries

## Primary boundaries

- API callers may request lifecycle operations but may not request arbitrary
  signatures or raw supervisor commands.
- The local backend may request only typed launch and lifecycle operations
  from its least-privilege headless process supervisor, never arbitrary commands.
- The signer is isolated from the local backend, the API daemon, the relay,
  supervised agent processes, and workspaces as far as the host permits.
- Supervised agent processes never receive a community identity private key or Server administrative credentials.
- Every agent belongs to exactly one configured community and receives only that
  community's relay URL and owner authorization.

## Owner signing

Server-native creation resolves the identity associated with the agent's community because each authorization is bound to a newly generated agent public key. Authorization construction and
verification call Buzz's shared `buzz-sdk` NIP-OA implementation so Desktop and
Server remain byte-for-byte compatible. The signer exposes only a structured,
policy-limited “authorize agent” operation—never arbitrary Nostr signing.

Community identities enter through `buzz-server communities join`. The CLI never places a private key in argv and never sends it through the lifecycle JSON API: interactive input is hidden and `--secret-file` is available for automation. Persistence uses KMS when configured; otherwise it mirrors Buzz Desktop by preferring the OS keyring and falling back to an owner-only local file. The daemon receives only a root-only ephemeral materialization under `/run/buzz-server/community-identities`, while the lifecycle API receives only the public key. Identical pubkeys are deduplicated across communities and the custody artifacts are removed when the last reference is deleted. Upgrades from the former single-owner configuration migrate that identity into this same per-community custody model before the new daemon starts.

## Required controls

- secret redaction in API, local-backend, process-supervisor, runtime, and audit
  logs;
- no plaintext community identity key, agent key, auth tag, or model credential in command
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

## Provider controls

Provider executables are trusted deployment plugins because the current Buzz protocol
may give them agent secrets. Buzz Server therefore requires explicit administrator
trust, path/hash pinning, bounded input/output, redacted logging, private staged
execution, and a restricted subprocess environment before invoking a provider.
