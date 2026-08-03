# ADR 0011: Define Server interface and lifecycle boundaries

## Status

Accepted; Compose-specific clause partially superseded by
[ADR 0012](0012-built-in-local-backend-first.md)

## Decision

Buzz Server follows Desktop protocol behavior but replaces the interactive interface with durable unattended operations.

### Administrative API

- Same-host callers may use a filesystem-protected Unix socket.
- Remote callers use TLS plus NIP-98 authenticated by an allowlisted pubkey; body-bearing requests bind the payload hash.
- Administrators may manage all resources. Draft submitters may create and inspect only their own non-secret drafts.
- Mutations require durable idempotency and record actor, action, resource, outcome, and correlation ID without secrets.

This is intentionally a fixed MVP authority model, not a general RBAC system.

### Local backend and supervisor

The MVP local backend owns desired lifecycle state and uses a least-privilege
headless process supervisor for launch, stop, inspect, logs, update, and delete.
The supervisor accepts typed, validated process and lifecycle requests, not
arbitrary shell commands. Durable launch receipts identify processes without
containing secrets.

Future provider v1 `info` and `deploy` calls may accept provider-specific
deployment configuration, but do not standardize later lifecycle calls.
Unsupported operations return an explicit capability error; a future extension
must be versioned.

### Crash recovery

| Interruption | Restart behavior |
| --- | --- |
| Before durable identity | Retry. |
| After identity, before authorization | Reuse the persisted key and continue. |
| After authorization, before process launch | Reuse authorization and stable launch identity. |
| After launch, before receipt persistence | Inspect and adopt the stable process. |
| During update or deletion | Reconcile toward durable desired state. |

Identity and launch identities are created once and reused across retries.

### Secret lifecycle

- The owner key is encrypted and decryptable only by the constrained signer; rotation requires reauthorization.
- Agent keys and runtime credentials are encrypted and released only to the
  target process as needed through an explicit, minimal launch context.
- Secrets never enter command arguments, ambient inherited environments, launch
  receipts, logs, or audit records.
- Backups remain encrypted; restore verifies derived agent pubkeys before starting services.
- Credential rotation preserves identity; agent-key replacement creates a new identity.
- Recoverable deletion stops compute; purge removes keys, credentials, and workspace but retains a non-secret tombstone and audit outcome.

The original Compose-specific secret clause is retained for future
implementations: a Compose provider must also keep secrets out of generated YAML
and project metadata.

## Consequences

The Server-specific behavior is explicit without changing Buzz/Nostr provider or relay protocols.
