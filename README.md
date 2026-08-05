# Buzz Server

Buzz Server is an optional headless Buzz client for creating and operating
always-on Buzz agents without depending on Buzz Desktop or a user laptop.

Buzz Server is not the Buzz relay and does not replace `buzz-acp`. The relay
remains the Nostr transport and shared-state authority. `buzz-acp` remains the
bridge between Buzz events and ACP-compatible runtimes.

## Implemented capabilities

- durable desired and observed agent lifecycle state;
- server-native agent identity generation and constrained NIP-OA authorization;
- direct supervision of `buzz-acp` and version-pinned ACP runtimes;
- restart-safe process receipts, adoption, reconciliation, and readiness;
- multiple explicitly configured, isolated communities and relays;
- authenticated Unix-socket and TLS/NIP-98 lifecycle API adapters;
- the `buzz-agentctl` lifecycle CLI;
- create, get/list, update, enable, disable, logs, recoverable delete, purge,
  operation polling, drafts, and draft promotion;
- trusted `buzz-backend-*` discovery, staged provider negotiation and deployment,
  capability validation, and pinned Kubernetes-provider compatibility fixtures;
- x86-64 and ARM64 release builds with a glibc 2.34 deployment baseline.

The built-in production deployment path is the durable local backend. External
provider compatibility is implemented, while selecting a provider as an agent's
durable lifecycle backend remains a later control-plane integration.

## Documentation

- [Lifecycle API](docs/LIFECYCLE_API.md)
- [`buzz-agentctl` CLI](docs/CLI.md)
- [Host deployment](deploy/README.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Security](docs/SECURITY.md)
- [Provider compatibility](docs/PROVIDER_COMPATIBILITY.md)
- [Compatibility with Buzz](docs/COMPATIBILITY_WITH_BUZZ.md)
- [Implementation milestones](docs/FOLLOW_UP_IMPLEMENTATION_PLAN.md)

## Current boundary

Milestones 1 through 4 are implemented. Production hardening remains Milestone 5:
encrypted owner custody and rotation, backup and restore exercises, stronger
resource and network restrictions, artifact provenance and rollback exercises,
and operational monitoring and alerts.
