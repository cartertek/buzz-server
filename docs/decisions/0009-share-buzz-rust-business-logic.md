# ADR 0009: Share Buzz Rust business logic

## Status

Accepted

## Decision

Buzz Server is a durable headless shell around the same protocol core as Buzz Desktop. Pin a reviewed Buzz revision and directly reuse `buzz-core`, `buzz-sdk`, and `buzz-ws-client`; in particular, use the shared NIP-OA implementation.

Extract pure provider request handling, deployment-payload serialization, runtime/config validation, and runtime catalog metadata into a Tauri-free shared crate where practical. Until then, use cross-implementation golden fixtures; do not copy or import the Desktop Tauri application crate.

Server-specific code implements the changed interface and operating model: authenticated API adapters, durable registry and operations, constrained signer IPC, server secret storage, reconciliation, supervisor receipts, concurrent community configurations, audit records, and optional drafts.

## Consequences

Desktop retains its interactive UI, OS keyring, app storage, and local-process integration. Server implements durable unattended operation while sharing protocol behavior by construction.
