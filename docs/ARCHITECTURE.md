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

Buzz Server supports multiple explicitly configured relay connections, matching
Buzz Desktop's multiple-community model. A connection binds a relay URL to an
owner identity/key reference. It is a client workspace boundary, not a Buzz
Server tenant: all agents, drafts, operations, credentials, workspaces, caches,
and background jobs are scoped by immutable `connection_id`. Cross-connection
behavior requires an explicit future bridge with separate authorization and
audit. Whether several URLs reach one physical relay is transparent to Server.

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
authorizing -> authorized -> provisioning -> running
                              \-> failed <-/
running -> updating | disabled | deleting
```

An authorized direct-create request enters this lifecycle immediately. Optional
agent drafts are separate, non-secret review resources, not lifecycle states.
They mint no identity, authorization, workspace, or deployment. Approval invokes
the same idempotent direct-create operation used by an authorized API caller.

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

The main API daemon does not receive unrestricted Docker access. A thin,
separately privileged helper implements only this supervisor contract; it is not
a general orchestration service or arbitrary command runner. The signer remains
separate from both processes.

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

The API contract is transport-independent and authenticated. A same-host
deployment may use a Unix socket with peer/filesystem authorization. Remote
administration uses a TLS network listener with explicit API authentication,
authorization, and audit. Neither transport assumes the relay is local.

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
POST   /v1/agent-drafts
GET    /v1/agent-drafts/{id}
POST   /v1/agent-drafts/{id}/deploy
```

Every agent and draft request names a `connection_id`; the server never relies
on a process-global active relay.

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
remain separate. Co-location is only a topology choice. Multiple relay
connections may run concurrently, and each agent receives only its connection's
relay URL and authorization. Compose project/service names, secret paths,
workspace paths, runtime state, and networks are isolated per agent and
connection.

## Readiness and deletion

`running` requires more than a running container: the expected agent public key,
valid owner authorization, successful connection to the configured relay, a
healthy `buzz-acp` process, and a successful harness-level probe must all agree.

Delete stops the agent immediately and enters recoverable retention. Secrets and
workspace are purged only after the configured retention policy expires. The
default retention duration remains an explicit product decision.

