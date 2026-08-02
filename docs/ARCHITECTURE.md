# Architecture

## Product boundary

Buzz Server is a headless Buzz client and agent operations control plane. It may
run on the relay host, as in the initial deployment, but communicates with the
relay over the same network/protocol boundary as other Buzz applications.

```text
administrative clients / future bridges
                  |
                  v
             Buzz Server
       API, policy, registry, reconciliation
         |                         |
         v                         v
 provider host + bundled      constrained owner
 self-hosted provider             signer
         |
         v
 supervisor interface -> Docker Compose driver (first)
         |
         v
 buzz-acp harness -> ACP runtime -> models/tools/workspace
         |
         v
               Buzz relay
```

The relay remains authoritative for Buzz events, channels, membership, and shared
conversation state. Buzz Server's registry is authoritative only for operational
desired state, deployment receipts, secrets references, and reconciliation.

## Terminology

- **Backend provider**: Buzz-compatible deployment adapter. Existing Desktop
  providers are executables named `buzz-backend-<id>` supporting `info` and
  `deploy` operations.
- **Self-hosted provider**: the bundled Buzz Server provider that converts an
  authorized agent deployment into a supervisor-neutral service specification.
- **Supervisor driver**: implementation that creates and keeps the service alive,
  such as Compose, systemd, Kubernetes, or a VM service.
- **Harness**: `buzz-acp`, which connects to the relay and manages ACP sessions.
- **ACP runtime**: the reasoning/tool process, such as `buzz-agent`, Codex ACP,
  Goose, or Claude ACP.
- **Signer**: isolated service permitted only to issue policy-constrained owner
  authorizations for new agents.

Docker Compose is a supervisor implementation, not a Buzz provider and not part
of the product-facing agent identity.

## Core components

### Control plane

Owns validation, policy, idempotent operations, desired-state transitions,
reconciliation, audit records, provider selection, and health aggregation.

Initial lifecycle:

```text
draft -> authorizing -> authorized -> provisioning -> running
                                  \-> failed <-/
running -> updating | disabled | deleting
```

Every mutating request receives an idempotency key and durable operation record.
Restarting Buzz Server must resume reconciliation without minting a second agent
identity or duplicating a deployment.

### Provider host

Discovers trusted executable providers, invokes `info`, validates their schemas,
and invokes `deploy` with bounded time/output and redacted logging. Installation
of providers is an administrator-only trust decision because the current Buzz
payload includes the agent private key and owner authorization.

The existing Buzz provider protocol is deploy-oriented. Status, logs, enable,
disable, and deletion are Buzz Server lifecycle operations. A future provider
protocol extension must be versioned or capability-negotiated rather than silently
changing the existing `info`/`deploy` contract.

### Bundled self-hosted provider

Consumes a fully authorized deployment and produces a `ServiceSpec` containing:

- immutable agent ID and stable Nostr identity;
- version-pinned harness/runtime package or image;
- `buzz-acp` command plus ACP runtime command and arguments;
- opaque secret references;
- persistent workspace and runtime-state mounts;
- resource, network, restart, and health policy.

Identity generation and owner signing remain control-plane responsibilities, not
provider or supervisor responsibilities.

### Supervisor interface

The initial behavioral interface is:

```text
apply(ServiceSpec) -> DeploymentReceipt
inspect(DeploymentReceipt) -> ObservedState
start(DeploymentReceipt)
stop(DeploymentReceipt)
delete(DeploymentReceipt, RetentionPolicy)
logs(DeploymentReceipt, Cursor)
```

The Compose driver renders configuration from registry state. Generated Compose
files are output, never the source of truth. Secrets must not be embedded in the
Compose YAML. Stable names derive from immutable internal agent IDs.

Future supervisor drivers may include Docker Engine, systemd, Swarm, Kubernetes,
and VM/job services. The interface should model capabilities rather than promise
that every driver supports identical behavior.

### Runtime catalog

Buzz Server should ultimately share or generate its runtime catalog from Buzz's
canonical definitions. “Supports Desktop runtimes” means compatible command,
argument, configuration, and packaging semantics; it does not imply every image
contains every runtime.

The Compose MVP supports the existing Codex deployment first. Runtime images and
adapters must be version-pinned. Additional runtimes follow after the catalog
sharing strategy is decided.

### API

The first API is private and local, preferably a Unix socket. A network listener
is disabled until authentication, TLS, authorization, and audit design are done.

Candidate resources:

```text
POST   /v1/agents
GET    /v1/agents
GET    /v1/agents/{id}
PATCH  /v1/agents/{id}
POST   /v1/agents/{id}/enable
POST   /v1/agents/{id}/disable
DELETE /v1/agents/{id}
GET    /v1/providers
GET    /v1/runtimes
GET    /v1/operations/{id}
```

Ordinary callers receive product-level controls. Arbitrary environment variables,
mounts, signing operations, and raw supervisor access are privileged administration.

Future bridges consume the API and Buzz events; they do not belong inside agent
lifecycle logic.

## Deployment topology

General topology:

```text
Host A: Buzz relay
Host B: Buzz Server, signer, and supervisor access
Host C..N: managed agents
```

Initial topology colocates Host A and B. Buzz Server and the signer should run as
separate hardened system services. The relay project and agent Compose project
remain separate. Longer term, a narrow privileged supervisor helper is preferable
to giving the main API daemon unrestricted Docker access.

