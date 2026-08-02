# MVP plan

## Goal

Create, verify, update, disable, re-enable, and delete an always-on server-hosted
Buzz agent from a name and day-to-day purpose, without requiring Buzz Desktop or
a client laptop to remain online.

## Phase 0 — protocol and threat-model spike

- inventory reusable Buzz types and isolate Desktop/Tauri coupling;
- document the provider v1 request/response contract at the pinned Buzz commit;
- reproduce owner authorization generation with disposable identities;
- define the server-native create request and exact NIP-OA signing policy;
- specify Unix-socket and remote TLS API authentication/authorization profiles;
- define community scoping and cross-community isolation invariants;
- decide naming/licensing/upstream relationship;
- define readiness and deletion-retention acceptance criteria.

Exit: reviewed decisions plus an executable disposable signing compatibility test.

## Phase 1 — local administrative control plane

- transport-independent authenticated API and companion CLI, with Unix socket as
  an optional same-host transport and TLS for remote administration;
- SQLite registry behind a repository interface;
- multiple explicit, isolated communities keyed by Server-local `community_id`;
- durable operation state machine and reconciliation loop;
- server-native agent key generation;
- separate disposable-key signer service;
- bundled self-hosted provider;
- discover trusted `buzz-backend-*` executables and implement provider v1
  `info`/`deploy` compatibility;
- Compose supervisor driver;
- thin privileged supervisor helper with no arbitrary command surface;
- version-pinned Codex ACP runtime image matching the current deployment;
- create, inspect, update, disable, enable, and recoverable delete;
- relay connection, owner authorization, and harness readiness verification;
- structured audit log with secret redaction;
- optional non-secret agent draft resources that promote through direct create.

Exit: a verified agent survives Buzz Server restart and reconciles without duplicate
identity or service creation.

## Phase 2 — production security and runtime parity

- KMS envelope encryption, audit, and kill switch;
- reviewed real-owner import ceremony;
- backup/restore and disaster-recovery exercise;
- runtime catalog sharing or generation from Buzz;
- additional Desktop-compatible ACP runtimes;
- resource/network hardening, upgrades, rollback, monitoring, and alerts.

## Phase 3 — provider hardening and protocol evolution

- accept already-signed Desktop-compatible deployments;
- sandbox provider subprocesses;
- define versioned lifecycle capability negotiation without breaking v1.

## Later phases

- richer identity and API authorization administration;
- PostgreSQL/high availability if single-instance SQLite becomes insufficient;
- remote supervisor nodes and more drivers;
- administrative UI and Desktop integration;
- Discord and other bridges as clients of the API/event system;
- broader headless Buzz Desktop functionality.

## Initial decisions to resolve

1. Recoverable workspace retention period after delete?
2. Bundled provider name: `self-hosted`, `managed-host`, or another term?
3. Can runtime definitions and deployment types move into a shared Buzz crate?
4. One multi-runtime image or runtime-specific images?
5. Exact harness-level readiness probe and timeout policy?
6. Exact remote API credential and authorization mechanism?
7. Authorization revocation semantics for already issued NIP-OA tags?
8. Independent companion, private deployment component, or upstream candidate?

## Quality gates

Once Rust tooling is available:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
cargo build --release
```

Use fake relays/signers and disposable identities in automated tests. Never place
the production owner identity in development or CI.

