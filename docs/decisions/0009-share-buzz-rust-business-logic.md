# ADR 0009: Share Buzz Rust business logic

## Status

Accepted

## Decision

Buzz Server is a durable headless shell around the same protocol core as Buzz Desktop. Pin a reviewed Buzz revision and directly reuse `buzz-core`, `buzz-sdk`, and `buzz-ws-client`; in particular, use the shared NIP-OA implementation.

Extract pure provider request handling, deployment-payload serialization, runtime/config validation, and runtime catalog metadata into a Tauri-free shared crate where practical. Until then, use cross-implementation golden fixtures; do not copy or import the Desktop Tauri application crate.

Server-specific code implements the changed interface and operating model: authenticated API adapters, durable registry and operations, constrained signer IPC, server secret storage, reconciliation, supervisor receipts, concurrent community configurations, audit records, and optional drafts.

The dependency lock records the exact reviewed Buzz commit for reproducible builds, but the pin is not a promise to remain stale. Automation checks Buzz `main` at least weekly and on every planned release, reports upstream commits affecting shared crates, provider contracts, runtimes, or protocol behavior, and opens a compatibility update. Dependency bumps are reviewed changes that run the shared fixtures and full test suite before merging. Implementation work begins by refreshing the pin, and long-lived feature branches rebase or re-audit against the current reviewed pin before merge.

## Consequences

Desktop retains its interactive UI, OS keyring, app storage, and local-process integration. Server implements durable unattended operation while sharing protocol behavior by construction.
