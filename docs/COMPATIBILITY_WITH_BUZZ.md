# Compatibility with Buzz

## Product boundary

Buzz Server is an optional headless Buzz client. It is not the relay and does not
replace `buzz-acp`.

The compatibility boundary follows Buzz's existing architecture:

- the relay transports and stores signed Nostr events;
- applications hold identities and perform authorized operations;
- Local execution and backend providers are distinct deployment choices;
- backend providers translate provider-backed deployments into external compute;
- `buzz-acp` bridges relay events to an ACP runtime;
- supervisors keep deployed processes alive.

Product documentation therefore uses “headless Buzz client” for Buzz Server and
reserves relay terminology for `buzz-relay`. “Headless Buzz Desktop” is a broader
direction rather than the current server contract.

## Shared Buzz behavior

Buzz Server relies on public Buzz behavior for:

- relay protocol, event persistence, membership, authorization, and client SDKs;
- `buzz-acp` relay-to-ACP harness and child runtime/session management;
- NIP-OA construction and verification through the shared `buzz-sdk`;
- access-policy and runtime/model configuration semantics;
- multiple-community semantics and community-scoped state;
- the `Local` versus `Provider { id, config }` deployment model;
- `buzz-backend-*` provider discovery and the provider v1 `info`/`deploy` wire contract;
- provider JSON configuration schemas and the upstream provider fixture corpus.

The repository pins a reviewed Buzz revision for reproducible builds. Compatibility
checks compare the pin with upstream movement, and dependency updates run the shared
fixtures and full test suite before merge.

## Buzz Server responsibilities

Buzz Server adds the durable unattended operating layer:

- persistent authenticated administrative APIs;
- server-native agent identities and constrained community-identity signing;
- desired-state registry and durable lifecycle operations;
- reconciliation, readiness, health, recovery, and audit behavior;
- a durable Server-native local backend modeled on Buzz Desktop Local semantics;
- headless process supervision for local agent child processes;
- remote lifecycle operations, logs, retention, backup, restore, and rotation;
- trusted provider-host discovery, negotiation, staging, and protocol compatibility.

Provider compatibility does not make an external provider the durable lifecycle
backend. The built-in local backend remains the operational lifecycle backend today.
Persisting `Provider { id, config }` in agent intent and routing lifecycle operations
through that backend is a separate control-plane integration; see
[Provider lifecycle integration](PROVIDER_LIFECYCLE_INTEGRATION.md).

## Compatibility decisions

1. The built-in durable local backend follows Buzz Desktop Local semantics. Local
   is not a `buzz-backend-*` provider.
2. Reuse or extract Tauri-free launch, configuration, runtime, and shared-type
   semantics; do not import the Desktop/Tauri application layer.
3. Keep local launch specifications, process supervision, receipts, and
   reconciliation as internal Server contracts. Do not call supervisor drivers
   providers.
4. Keep external provider compatibility separate from the built-in local backend.
   Trusted discovery and provider v1 `info`/`deploy` compatibility are implemented;
   provider-backed lifecycle selection remains a separate integration.
5. Keep signing before the external-provider boundary. Providers receive an
   already authorized deployment, matching Desktop's trust model. Existing
   providers are deploy-only unless they advertise a versioned lifecycle capability.
6. Keep operational secrets and desired state in Buzz Server. Publish only
   appropriate non-secret lifecycle/audit information as signed Buzz events.
7. Keep ACP runtime/model configuration distinct from deployment-backend selection.
8. Keep Buzz Server deployable independently of the relay host.
9. Scope every agent and all operational state to one explicit community
   configuration, matching Desktop's community boundary.
10. Communicate with relays as an ordinary Buzz client through configured URLs and
    standard Buzz/Nostr authentication and APIs.

## Sources

- [Buzz architecture](https://github.com/block/buzz/blob/main/ARCHITECTURE.md)
- [Backend provider implementation](https://github.com/block/buzz/blob/main/desktop/src-tauri/src/managed_agents/backend.rs)
- [Managed-agent backend types](https://github.com/block/buzz/blob/main/desktop/src-tauri/src/managed_agents/types.rs)
- [Deployment payload builder](https://github.com/block/buzz/blob/main/desktop/src-tauri/src/commands/agents_deploy.rs)
- [`buzz-acp` guide](https://github.com/block/buzz/blob/main/crates/buzz-acp/README.md)
- [Desktop community-switching guidance](https://github.com/block/buzz/blob/main/AGENTS.md#community-switching)
