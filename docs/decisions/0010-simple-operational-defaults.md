# ADR 0010: Adopt simple operational defaults

## Status

Accepted; Compose-specific MVP clause partially superseded by
[ADR 0012](0012-built-in-local-backend-first.md)

## Decision

The MVP uses one active Server/reconciler, SQLite WAL, numbered transactional
forward migrations, and encrypted backup before migration. Runtime artifacts are
pinned by immutable version or digest. Upgrades are explicit and backup-first.
Future provider executables are allowlisted and checksum-pinned.

Community onboarding normalizes and deduplicates relay URLs, verifies required capabilities through ordinary Buzz protocols, and confirms owner authority. Recoverable deletion retains identity and workspace for 30 days by default, configurable per installation, followed by an idempotent daily purge.

Remote supervisor nodes and detailed per-agent resource, UID, and egress policies
are deferred. The MVP local backend directly launches each agent through a
least-privilege headless process-supervisor boundary. That boundary permits only
validated executables, arguments, environment keys, paths, signals, and lifecycle
operations; uses distinct workspace and secret paths; and denies agents the
owner key and Server administrative credentials.

The original MVP required a narrow helper to invoke Compose. ADR 0012 moves
Compose out of the first deployment path. A future Compose provider must retain
the narrow-helper and no-Docker-socket controls.

## Consequences

The first implementation has explicit safe defaults without prematurely designing high availability or a remote node protocol.
