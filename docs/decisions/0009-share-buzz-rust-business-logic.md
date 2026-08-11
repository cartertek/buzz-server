# ADR 0009: Share Buzz Rust business logic

## Status

Accepted

## Decision

Buzz Server is a durable headless shell around the same protocol core as Buzz Desktop. Pin a reviewed Buzz revision and directly reuse `buzz-core`, `buzz-sdk`, and `buzz-ws-client`; in particular, use the shared NIP-OA implementation.

For the MVP, extract or directly reuse Tauri-free shared types and pure launch,
configuration, runtime, model-configuration, and credential-reference semantics
from Buzz Desktop Local where practical. Desktop Local is a deployment backend,
not a `buzz-backend-*` provider. Do not copy or import the Desktop Tauri application
crate.

Server-specific code implements the durable headless operating model: authenticated
API adapters, registry and operations, constrained signer IPC, server secret
storage, an internal local launch specification and headless process-supervisor
contracts, reconciliation, process receipts, concurrent community configurations,
audit records, and optional drafts.

External provider discovery and provider v1 request handling were originally
deferred from the first local deployment path, as was a possible Docker Compose
provider. Provider discovery and provider v1 compatibility have since been
implemented; Docker Compose remains deferred. ACP runtimes and their model API
providers/configuration remain distinct from deployment backends and external
backend providers.

The dependency lock records the exact reviewed Buzz commit for reproducible builds, but the pin is not a promise to remain stale. Automation checks Buzz `main` at least weekly and on every planned release, reports upstream commits affecting shared crates, provider contracts, runtimes, or protocol behavior, and opens a compatibility update. Dependency bumps are reviewed changes that run the shared fixtures and full test suite before merging. Implementation work begins by refreshing the pin, and long-lived feature branches rebase or re-audit against the current reviewed pin before merge.

## Consequences

Desktop retains its interactive UI, OS keyring, app storage, and local-process
integration. Server implements durable unattended local execution while sharing
Tauri-free protocol and launch/configuration behavior by construction. The local
launch specification, process supervision, and reconciliation contracts
remain Server-internal. Container-oriented fields and supervisors are future
extensions.
