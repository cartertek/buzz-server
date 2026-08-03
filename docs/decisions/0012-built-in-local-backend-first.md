# ADR 0012: Use the built-in local backend path first

## Status

Accepted

## Decision

The MVP uses a durable, Server-native local backend modeled on Buzz Desktop's
built-in `Local` path. `Local` is not a `buzz-backend-*` provider: it launches
`buzz-acp` and the selected ACP runtime directly under application-owned process
supervision.

Buzz Server reuses or extracts Tauri-free launch, configuration, runtime, and
shared type semantics from Buzz where practical. It does not import the Desktop
Tauri application. Server-specific code owns unattended process supervision,
durable launch receipts, desired and observed state, reconciliation after
restart, secret release, audit, and lifecycle operations.

External `buzz-backend-*` discovery and provider v1 `info`/`deploy`
compatibility remain planned extensions rather than dependencies of the first
vertical slice. A Docker Compose provider may be added as one such optional
deployment path.

## Consequences

The first agent path has no provider subprocess or provider-selection hop. This
reduces the initial integration surface while preserving compatibility with
Buzz's local runtime semantics.

The distinction between providers and supervisors remains in force. Future
providers adapt authorized deployments to external compute; supervision keeps a
launched service alive. Adding a Docker Compose provider or another provider
does not make Compose part of the built-in local backend's identity.

This decision supersedes ADR 0004 and only the bundled-provider/Compose MVP
clause of ADR 0002. It does not supersede ADR 0002's general separation of
providers from supervisors.
