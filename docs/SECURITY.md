# Security and trust boundaries

## Primary boundaries

- API callers may request lifecycle operations but may not request arbitrary
  signatures or raw supervisor commands.
- Provider executables are fully trusted deployment plugins. The current Buzz
  protocol gives them agent secrets, so installation is administrator-only.
- The self-hosted provider receives an already authorized deployment.
- The signer is isolated from providers, the API daemon, the relay, agent
  containers, Docker access, and workspaces as far as the host permits.
- Agent containers never receive the owner private key.

## Owner signing

Server-native creation requires the owner identity because each authorization is
bound to a newly generated agent public key. The signer exposes only a structured,
policy-limited “authorize agent” operation—never arbitrary Nostr signing.

Development begins with a disposable owner identity. Production key import occurs
only after review of event construction, policy, logging, isolation, recovery, and
revocation.

Envelope encryption with KMS protects offline ciphertext, backups, and snapshots;
provides audit records and a kill switch; and limits decrypt permission to the
signer. It does not protect against a fully compromised live host that can invoke
or inspect the authorized signer.

## Required controls

- secret redaction in API, provider, supervisor, and audit logs;
- no plaintext owner key, agent key, auth tag, or model credential in registry
  metadata or generated Compose YAML;
- stable agent keys across ordinary updates;
- immediate signer/KMS disable path;
- encrypted off-host backup and tested restoration;
- rate, replay, capability, expiry, and caller checks in the signer;
- memory-dump prevention and hardened service accounts;
- key versioning, rotation, revocation, and reauthorization runbooks;
- idempotent operations to prevent duplicate identity creation;
- explicit deletion retention policy; no implicit workspace destruction.

## Open threat-model questions

- single-owner versus multi-owner data model;
- API authentication for remote administration;
- provider subprocess sandboxing;
- narrow supervisor helper versus direct Docker access;
- agent-to-signer network and filesystem isolation;
- authorization revocation semantics for already issued NIP-OA tags;
- identity attribution and membership policy for future external bridges.

