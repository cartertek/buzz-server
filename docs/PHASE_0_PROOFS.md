# Phase 0 empirical proofs

Evidence captured against `block/buzz` commit `7ff5fc31895efe6265a379d01637c8ee301872e5`, which was current `main` when this proof set ran.

## 1. Upstream revision and provider audit

- `Cargo.toml` and `Cargo.lock` pin `buzz-sdk` to the exact reviewed commit.
- `scripts/check-buzz-upstream.sh` compares that pin to current Buzz `main`. It is ready for weekly CI; adding the scheduled workflow requires a repository credential with GitHub workflow-write scope.
- Current Buzz includes the formal `docs/remote-agents.md` specification, the `buzz-backend-kubernetes` provider, golden provider-wire fixtures, and the published `ghcr.io/block/buzz-sprig` remote-agent image.
- Kubernetes is a reference binding, not the Compose implementation required by the first Buzz Server deployment.
- At the pinned commit, all 22 filtered upstream NIP-OA tests and all 4 built-binary Kubernetes provider wire-fixture tests pass.

## 2. Shared-code boundary

Reuse now:

- `buzz-sdk` NIP-OA implementation, pinned directly;
- the upstream provider-wire fixture as a golden compatibility arbiter;
- `buzz-core` and `buzz-ws-client` directly when their first production call sites are added;
- the Sprig multicall binary/image artifacts for `buzz-acp`, `buzz-agent`, `buzz-dev-mcp`, Buzz CLI, and Nostr Git helpers.

Propose upstream extraction:

- a Tauri-free provider-protocol crate containing request/response types, validation, and fixtures currently split between Desktop and `buzz-backend-kubernetes`;
- later, a runtime-definition crate containing stable descriptors and effective launch configuration.

Keep in Buzz Server:

- API authorization and transport adapters;
- SQLite registry, operations, reconciliation, and audit;
- signer IPC and server-side secret storage;
- supervisor drivers and deployment receipts;
- concurrent community scoping.

Do not import the Desktop Tauri crate. Runtime discovery, OS keyrings, app paths, GUI progress, and local child-process ownership remain Desktop adapters.

## 3. NIP-OA compatibility

`tests/nip_oa_compat.rs` calls the exact pinned `buzz-sdk` implementation that Desktop calls. It:

- verifies Buzz’s published owner-attestation vector;
- creates and verifies an authorization using fixed disposable keys;
- proves the harness tag parser accepts the output;
- proves noncanonical conditions are rejected.

This removes custom NIP-OA construction from the Server implementation plan. Renewal means issuing a new shared-SDK tag; owner rotation requires reauthorization. Revocation remains relay/owner policy rather than a new tag format invented by Server.

## 4. Readiness proof and timeout

Current Buzz exposes two useful signals:

1. `buzz-acp models` performs ACP spawn, initialize, and session creation with a 10-second internal timeout.
2. A running harness publishes signed kind-20001 `online` presence after relay connection; remote-agent presence expires after 180 seconds.

The first Compose vertical slice therefore uses this exact acceptance sequence:

1. Before apply, run `buzz-acp models` inside the selected runtime image with the resolved command, arguments, and non-secret runtime environment. Allow 15 seconds externally. Exit zero proves the ACP runtime initializes without sending a conversational turn.
2. Apply the service and require an `online` presence event signed by the expected agent pubkey on the configured community within 30 seconds.
3. Mark the service ready only if supervisor state is healthy, the stored NIP-OA tag verifies, the preflight passed, and expected presence arrived.
4. After readiness, loss of presence marks the agent degraded. Do not restart solely for relay loss; reconnect is the harness’s job. Mark it offline after the relay’s 180-second presence TTL.

The vertical-slice integration test may adjust the 30-second initial window if measured startup data demonstrates it is insufficient; changing it requires recording the measurement.

## 5. Runtime packaging

Buzz’s Sprig image is a small remote-agent image containing the Sprig multicall binary and symlinks for `buzz-acp`, `buzz-agent`, `buzz-dev-mcp`, `buzz`, `rg`, `tree`, and Nostr Git helpers. It does not bundle every external ACP runtime.

Decision: use runtime-specific images, not one ever-growing multi-runtime image.

- The built-in Buzz Agent path can use the digest-pinned upstream Sprig image directly.
- The initial Codex path uses a separately digest-pinned Codex runtime image containing the reviewed Sprig/Buzz binaries plus the exact Codex CLI and `codex-acp` versions.
- Claude and Goose receive separate images only when implemented.
- A runtime catalog maps runtime ID to image digest, command, arguments, probe, and required secret references.

This keeps credentials, dependencies, upgrades, and rollback isolated per runtime.

## 6. Upstream and licensing mechanics

Both Buzz and Buzz Server are Apache-2.0. Direct dependencies retain their upstream license metadata. Vendored fixtures carry source path and revision attribution. Modified upstream files, if any are ever copied, must carry a prominent change notice and retain applicable notices.

Shared protocol/runtime extraction should be proposed upstream first as behavior-preserving changes with fixtures. Buzz Server may consume a reviewed fork revision while a contribution is pending, but must not maintain an untracked permanent fork. The Server can proceed against public APIs and golden fixtures while upstream review occurs.

This is an engineering policy summary, not legal advice.

## Result

The six Phase 0 questions are resolved sufficiently to begin implementation. Remaining uncertainty is measured inside the first vertical slice rather than represented as an architecture choice.
