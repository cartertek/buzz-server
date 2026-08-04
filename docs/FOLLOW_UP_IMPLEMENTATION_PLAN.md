# Follow-up implementation plan

## Milestone 1: shared foundations

Status: complete. The domain types, SQLite WAL repositories and migrations,
attributed audit/idempotency records, direct pinned Buzz dependencies, Local
runtime catalog, and `buzz-acp` launch/receipt contracts pass the milestone
restart, compatibility, freshness, and full-package gates.

- Introduce library modules for IDs, `CommunityConfig`, agent specifications, operation states, and stable API errors.
- Add SQLite WAL storage, numbered migrations, repository interfaces, audit records, and idempotency keys.
- Add direct `buzz-core` and `buzz-ws-client` dependencies at the same reviewed Buzz revision.
- Wire `scripts/check-buzz-upstream.sh` into weekly CI once the repository credential has GitHub workflow-write scope.
- Identify and reuse or extract Tauri-free Desktop local-launch, configuration, runtime, and shared type semantics without importing Desktop/Tauri.
- Add the runtime catalog with digest-pinned Sprig and Codex entries.

Exit: migrations and repositories pass restart/idempotency tests; dependency and fixture checks are green.

## Milestone 2: disposable vertical slice

- Configure one community through its authoritative relay URL.
- Generate a disposable owner and agent identity; issue NIP-OA through `buzz-sdk`.
- Implement constrained signer IPC for the one authorize-agent operation.
- Implement the durable Server-native local backend, launch receipt, and
  reconciliation around `buzz-acp` and its ACP runtime.
- Build the first version-pinned Codex runtime package and run the 15-second ACP preflight.
- Deploy one agent, observe expected signed presence within 30 seconds, restart Server, and adopt the same service and identity.

Exit: no duplicate identity or service across injected restart points; the agent is reachable through the relay.

## Milestone 3: lifecycle API

- Add authenticated Unix-socket and TLS/NIP-98 adapters over one application service.
- Implement create, get/list, update, enable, disable, logs, recoverable delete, purge, and durable operation polling.
- Add fixed administrator and draft-submitter authority classes and redacted audit attribution.
- Add optional drafts that promote through the same direct-create service.

Exit: full lifecycle contract passes API, reconciliation, retention, and audit tests.

## Milestone 4: provider compatibility

- Add trusted `buzz-backend-*` discovery and staged `info`/`deploy` invocation.
- Evaluate a Docker Compose provider as an optional external deployment path.
- Verify the full upstream fixture corpus and explicit unsupported lifecycle behavior.
- Reuse or consume the proposed upstream provider-protocol crate when available.

Exit: the Kubernetes reference provider and a fake provider pass the compatibility suite without receiving secrets before negotiation.

## Milestone 5: production hardening

- Replace disposable owner custody with reviewed encrypted import and KMS envelope encryption.
- Exercise encrypted backup/restore, owner rotation, reauthorization, retention, and purge.
- Add resource/network restrictions, artifact provenance verification, upgrades/rollback, monitoring, and alerts.
- Add additional runtime-specific artifacts only through catalog entries and readiness fixtures.

Exit: production threat-model checklist and disaster-recovery exercise pass.

## Work ordering

Milestones are sequential at their acceptance boundaries, but schema/API work,
signer IPC, local-backend supervision, and runtime-package work can proceed in
parallel once Milestone 1 types and fixtures are stable.
