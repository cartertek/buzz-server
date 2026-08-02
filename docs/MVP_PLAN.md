# MVP plan

## Goal

Create, verify, update, disable, re-enable, and delete an always-on server-hosted
Buzz agent from a name and day-to-day purpose, without requiring Buzz Desktop or
a client laptop to remain online.

## Phase 0 — protocol and threat-model spike (completed)

- depend directly on pinned `buzz-sdk` and identify the first production call sites for `buzz-core` and `buzz-ws-client`;
- inventory Tauri-free managed-agent logic suitable for an upstream shared crate;
- document the provider v1 request/response contract at the pinned Buzz commit;
- prove Desktop/Server NIP-OA parity with shared fixtures;
- define only the durable signing lifecycle Server adds around shared NIP-OA;
- specify Unix-socket and remote TLS API authentication/authorization profiles;
- define community scoping and cross-community isolation invariants;
- decide naming/licensing/upstream relationship;
- define readiness and deletion-retention acceptance criteria.

Exit satisfied at Buzz revision `7ff5fc31895efe6265a379d01637c8ee301872e5`; see [PHASE_0_PROOFS.md](PHASE_0_PROOFS.md).

## Phase 1 — local administrative control plane

- transport-independent authenticated API and companion CLI, with Unix socket as
  an optional same-host transport and TLS for remote administration;
- SQLite registry behind a repository interface;
- multiple explicit, isolated `CommunityConfig` records referenced by local
  `community_config_id`, each with one authoritative relay URL;
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

## Phase 0 outputs

The six proofs, decisions, source evidence, and measured follow-up seams are recorded in [PHASE_0_PROOFS.md](PHASE_0_PROOFS.md). Implementation milestones and acceptance boundaries are recorded in [FOLLOW_UP_IMPLEMENTATION_PLAN.md](FOLLOW_UP_IMPLEMENTATION_PLAN.md).

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

