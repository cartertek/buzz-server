# ADR 0011: Define Server interface and lifecycle boundaries

## Status

Accepted

## Decision

Buzz Server follows Desktop protocol behavior but replaces the interactive interface with durable unattended operations.

### Administrative API

- Same-host callers may use a filesystem-protected Unix socket.
- Remote callers use TLS plus NIP-98 authenticated by an allowlisted pubkey; body-bearing requests bind the payload hash.
- Administrators may manage all resources. Draft submitters may create and inspect only their own non-secret drafts.
- Mutations require durable idempotency and record actor, action, resource, outcome, and correlation ID without secrets.

This is intentionally a fixed MVP authority model, not a general RBAC system.

### Provider and supervisor

The supervisor owns running-service lifecycle. Full start, stop, inspect, logs, update, and delete are available when Server controls or has an explicit interface to that supervisor. Provider v1 `info` and `deploy` may accept provider-specific deployment configuration, but do not standardize later lifecycle calls. Unsupported operations return an explicit capability error; a future extension must be versioned.

### Crash recovery

| Interruption | Restart behavior |
| --- | --- |
| Before durable identity | Retry. |
| After identity, before authorization | Reuse the persisted key and continue. |
| After authorization, before supervisor apply | Reuse authorization and stable deployment name. |
| After apply, before receipt persistence | Inspect and adopt the stable deployment. |
| During update or deletion | Reconcile toward durable desired state. |

Identity and deployment names are created once and reused across retries.

### Secret lifecycle

- The owner key is encrypted and decryptable only by the constrained signer; rotation requires reauthorization.
- Agent keys and runtime credentials are encrypted and released only to the deployment path and target agent as needed.
- Secrets never enter Compose YAML, logs, or audit records.
- Backups remain encrypted; restore verifies derived agent pubkeys before starting services.
- Credential rotation preserves identity; agent-key replacement creates a new identity.
- Recoverable deletion stops compute; purge removes keys, credentials, and workspace but retains a non-secret tombstone and audit outcome.

## Consequences

The Server-specific behavior is explicit without changing Buzz/Nostr provider or relay protocols.
