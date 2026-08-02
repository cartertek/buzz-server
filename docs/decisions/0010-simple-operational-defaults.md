# ADR 0010: Adopt simple operational defaults

## Status

Accepted

## Decision

The MVP uses one active Server/reconciler, SQLite WAL, numbered transactional forward migrations, and encrypted backup before migration. Artifacts are pinned by immutable version or digest; provider executables are allowlisted and checksum-pinned. Upgrades are explicit and backup-first.

Community onboarding normalizes and deduplicates relay URLs, verifies required capabilities through ordinary Buzz protocols, and confirms owner authority. Recoverable deletion retains identity and workspace for 30 days by default, configurable per installation, followed by an idempotent daily purge.

Remote supervisor nodes and detailed per-agent resource, UID, and egress policies are deferred. The MVP still forbids agent access to the Docker socket, owner key, or Server admin credentials; uses distinct workspace and secret paths; and lets only the narrow helper invoke Compose.

## Consequences

The first implementation has explicit safe defaults without prematurely designing high availability or a remote node protocol.
