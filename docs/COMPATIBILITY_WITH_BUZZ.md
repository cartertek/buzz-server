# Compatibility with Buzz

## Assessment

The proposal is compatible with Buzz when Buzz Server is described as an optional
headless Buzz client rather than the relay or a replacement for
`buzz-acp`.

This follows existing Buzz boundaries:

- the relay transports and stores signed Nostr events;
- applications hold identities and perform authorized operations;
- Local execution and backend providers are distinct deployment choices;
- backend providers translate provider-backed deployments into external compute;
- `buzz-acp` bridges relay events to an ACP runtime;
- supervisors keep deployed processes alive.

The name **Buzz Server** can be confused with the Buzz relay, which existing
documentation also calls the server. Product documentation must consistently say
“headless Buzz client” and preserve `buzz-relay` as the protocol/shared-state
authority. “Headless Buzz Desktop” is a long-term direction, not the v0 contract.

## Already implemented by public Buzz

- relay protocol, event persistence, membership, authorization, and client SDKs;
- `buzz-acp` relay-to-ACP harness and child runtime/session management;
- Desktop-managed agent identity generation and owner authorization;
- access policy and runtime/model configuration;
- Desktop's built-in local process supervision;
- Desktop's multiple-community model, with each community backed by a relay and
  community-scoped state reset at relay-boundary changes;
- `Local` versus `Provider { id, config }` backend model;
- `buzz-backend-*` discovery and the `info`/`deploy` executable protocol;
- a public Kubernetes backend provider on current Buzz `main`, pending exact revision pinning and audit;
- provider JSON configuration schemas and provider-aware Desktop UI;
- complete remote deployment payload construction;
- ACP runtime command and argument configuration.

## New work in Buzz Server

- persistent headless administrative API;
- server-native agent identity and constrained owner signer;
- desired-state registry and durable lifecycle operations;
- reconciliation, readiness, health, recovery, and audit behavior;
- durable Server-native local backend modeled on Desktop Local semantics;
- durable headless process supervision for local agent child processes;
- remote update, status, enable, disable, deletion, logs, and retention behavior;
- secure secret storage and rotation procedures;
- future provider-host functionality outside Desktop;
- future bridges such as Discord.

The earlier inventory was made at Buzz commit `b1b283cd4c7f926e12eeee8ae1f38c7471922b16`. Phase 0 refreshed it against commit `7ff5fc31895efe6265a379d01637c8ee301872e5`, which supplied the NIP-OA and provider-wire fixtures. The dependency pin was reviewed again at `a5dbdf5e61e4c512acd99c219c79c154ddb57295`; the intervening change affects only the mobile relay client, so the shared Rust crates and recorded fixtures are unchanged.

Milestone 4 refreshed the exact dependency pin to
`0afeac8a7c173fd3ede8a22e27919e63161bf07c`. The commits since the preceding
pin affect only Desktop profile/sidebar UI and managed-agent restart-diff
presentation/state; shared Rust crates and the Kubernetes provider
contract/fixtures are unchanged.

## Compatibility decisions

1. Implement the MVP as a durable Server-native local backend modeled on Buzz
   Desktop Local. Local is not a `buzz-backend-*` provider.
2. Reuse or extract Tauri-free launch, configuration, runtime, and shared-type
   semantics; do not import the Desktop/Tauri application layer.
3. Keep the local launch specification, process supervision, receipts, and
   reconciliation as internal Server contracts. Do not call supervisor drivers
   providers.
4. Defer external provider discovery, provider v1 `info`/`deploy` compatibility,
   container/Compose supervision, and a possible Docker Compose provider until
   after the local-process MVP.
5. Keep the signer before any future external-provider boundary; providers receive
   an already authorized deployment, matching Desktop's trust model. Treat existing
   providers as deploy-only unless they advertise a future, versioned lifecycle
   capability.
6. Keep operational secrets and desired state in Buzz Server. Publish only
   appropriate non-secret lifecycle/audit information as signed Buzz events.
7. Support generic ACP runtime configuration while working toward a shared
   canonical runtime catalog instead of maintaining a drifting copy. An ACP
   runtime and its model API provider/configuration are distinct from the
   deployment backend and any backend provider.
8. Keep Buzz Server deployable independently of the relay host.
9. Support multiple explicit communities as isolated client workspaces, not as a
   multi-tenant control-plane namespace. Scope every agent and all operational
   state by Server-local `community_config_id`, following Desktop's community
   boundary. The authoritative relay URL is the shared community locator.
10. Communicate with every relay exactly as another Buzz client: use its configured
   URL, standard NIP-42/NIP-98 authentication, and ordinary Buzz/Nostr APIs. Do
   not require a tenant-administration API, relay database access, special
   headers, or co-location.

## Sources

- [Buzz architecture](https://github.com/block/buzz/blob/main/ARCHITECTURE.md)
- [Backend provider implementation](https://github.com/block/buzz/blob/main/desktop/src-tauri/src/managed_agents/backend.rs)
- [Managed-agent backend types](https://github.com/block/buzz/blob/main/desktop/src-tauri/src/managed_agents/types.rs)
- [Deployment payload builder](https://github.com/block/buzz/blob/main/desktop/src-tauri/src/commands/agents_deploy.rs)
- [`buzz-acp` guide](https://github.com/block/buzz/blob/main/crates/buzz-acp/README.md)
- [Desktop community-switching guidance](https://github.com/block/buzz/blob/main/AGENTS.md#community-switching)
