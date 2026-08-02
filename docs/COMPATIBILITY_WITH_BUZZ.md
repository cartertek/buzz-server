# Compatibility with Buzz

## Assessment

The proposal is compatible with Buzz when Buzz Server is described as an optional
headless application/control plane rather than the relay or a replacement for
`buzz-acp`.

This follows existing Buzz boundaries:

- the relay transports and stores signed Nostr events;
- applications hold identities and perform authorized operations;
- backend providers translate deployments into external compute;
- `buzz-acp` bridges relay events to an ACP runtime;
- supervisors keep deployed processes alive.

The name **Buzz Server** can be confused with the Buzz relay, which existing
documentation also calls the server. Product documentation must consistently say
“headless control plane” and preserve `buzz-relay` as the protocol/shared-state
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
- provider JSON configuration schemas and provider-aware Desktop UI;
- complete remote deployment payload construction;
- ACP runtime command and argument configuration.

## New work in Buzz Server

- persistent headless administrative API;
- server-native agent identity and constrained owner signer;
- desired-state registry and durable lifecycle operations;
- reconciliation, readiness, health, recovery, and audit behavior;
- shipped self-hosted backend provider;
- supervisor abstraction and Compose driver;
- remote update, status, enable, disable, deletion, logs, and retention behavior;
- secure secret storage and rotation procedures;
- provider-host functionality outside Desktop;
- future bridges such as Discord.

These “not implemented” claims are scoped to the tracked public Buzz tree and
official files inspected at Buzz commit `b1b283cd4c7f926e12eeee8ae1f38c7471922b16`.

## Compatibility decisions

1. Mirror the existing provider v1 `info` and `deploy` contract where possible.
2. Do not call supervisor drivers providers.
3. Keep the signer before the provider boundary; providers receive an already
   authorized deployment, matching Desktop's trust model.
4. Treat existing providers as deploy-only unless they advertise a future,
   versioned lifecycle capability.
5. Keep operational secrets and desired state in Buzz Server. Publish only
   appropriate non-secret lifecycle/audit information as signed Buzz events.
6. Support generic ACP runtime configuration while working toward a shared
   canonical runtime catalog instead of maintaining a drifting copy.
7. Keep Buzz Server deployable independently of the relay host.
8. Support multiple explicit communities as isolated client workspaces, not as a
   multi-tenant control-plane namespace. Scope every agent and all operational
   state by Server-local `community_config_id`, following Desktop's community
   boundary. The authoritative relay URL is the shared community locator.

## Sources

- [Buzz architecture](https://github.com/block/buzz/blob/main/ARCHITECTURE.md)
- [Backend provider implementation](https://github.com/block/buzz/blob/main/desktop/src-tauri/src/managed_agents/backend.rs)
- [Managed-agent backend types](https://github.com/block/buzz/blob/main/desktop/src-tauri/src/managed_agents/types.rs)
- [Deployment payload builder](https://github.com/block/buzz/blob/main/desktop/src-tauri/src/commands/agents_deploy.rs)
- [`buzz-acp` guide](https://github.com/block/buzz/blob/main/crates/buzz-acp/README.md)
- [Desktop community-switching guidance](https://github.com/block/buzz/blob/main/AGENTS.md#community-switching)

