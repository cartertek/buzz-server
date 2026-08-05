# Architecture

## Product boundary

Buzz Server is a headless Buzz client. Internally, it provides the API, policy,
registry, and reconciliation needed to operate agents. It communicates with the
relay over the same network and protocol boundary as other Buzz clients.

```text
administrative clients / future bridges
                  |
                  v
             Buzz Server
       API, policy, registry, reconciliation
         |                         |
         v                         v
 durable local backend        constrained owner
                                 signer
         |
         v
 headless process supervisor
         |
         v
 buzz-acp/runtime child process -> models/tools/workspace
         |
         v
               Buzz relay
```

The relay remains authoritative for Buzz events, channels, membership, and shared
conversation state. Buzz Server's registry is authoritative only for operational
desired state, deployment receipts, secrets references, and reconciliation.

Buzz Server supports multiple explicitly configured communities, matching Buzz
Desktop's model. A `CommunityConfig` has a Server-local UUID `id`, display name,
one authoritative `relay_url`, and an owner identity/key reference. The relay URL
is the shared community locator used by all clients; the local ID only identifies
Server's saved configuration and is stored as `community_config_id` in foreign
keys. Agents, drafts, operations, credentials, workspaces, caches, and jobs are
scoped to that configuration. Cross-community behavior requires an explicit
future bridge with separate authorization and audit.

## Terminology

- **Deployment backend**: the choice between Desktop-style `Local` execution and
  an external `Provider { id, config }` deployment. The MVP implements a
  Server-native durable local backend; Desktop Local is not a `buzz-backend-*`
  provider.
- **Backend provider**: an external Buzz-compatible deployment adapter. Existing
  providers are executables named `buzz-backend-<id>` supporting `info` and
  `deploy` operations. Trusted discovery and protocol compatibility are
  implemented; durable lifecycle-backend selection is a later integration.
- **Process supervisor**: the Server component that launches and keeps the local
  `buzz-acp`/runtime child process alive, including process-group, restart, and
  health handling.
- **Harness**: `buzz-acp`, which connects to the relay and manages ACP sessions.
- **ACP runtime**: the reasoning/tool process, such as `buzz-agent`, Codex ACP,
  Goose, or Claude ACP.
- **Model API provider**: the service selected through an ACP runtime's model and
  credential configuration. It is not a deployment backend or backend provider.
- **Signer**: isolated service permitted only to issue policy-constrained owner
  authorizations for new agents.

Docker Compose, containers, and remote supervisors are future execution options,
not part of the local MVP or the product-facing agent identity.

## Core components

### Control plane

Owns validation, policy, idempotent operations, desired-state transitions,
reconciliation, audit records, deployment-backend selection, and health
aggregation.

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

### Durable local backend

The MVP models Buzz Desktop Local semantics in a Server-native durable backend.
It resolves shared launch, runtime, model-configuration, and credential semantics
into an internal launch specification, then reconciles that specification through
the headless process supervisor. It reuses or extracts Tauri-free shared types and pure
configuration logic where practical, but does not import Desktop/Tauri or inherit
Desktop's GUI, OS-keyring, app-path, or local-child-process ownership adapters.

The launch specification, process receipts, supervisor operations, and
reconciliation are internal Buzz Server contracts rather than a new public
provider protocol.
Identity generation and owner signing remain control-plane responsibilities, not
backend or supervisor responsibilities.

### External provider compatibility

Buzz Server implements trusted `buzz-backend-*` discovery and provider v1
`info`/`deploy` compatibility. Provider bytes are bound to an administrator
approved path and SHA-256, copied into sealed private execution storage, and
negotiated before a secret-bearing deployment payload can be constructed.
Installing a provider is an administrator trust decision because the current Buzz
payload includes the agent private key and owner authorization.

The existing provider protocol is deploy-oriented. Additional lifecycle actions
are invoked only when advertised through the independently versioned capability
contract. Unsupported actions are explicit and are never silently assigned new
meaning under provider protocol v1.

The built-in local backend remains the operational lifecycle backend. Persisting
`Provider { id, config }` in agent intent and routing lifecycle operations through
that backend is a later control-plane integration. Container execution, a Compose
supervisor driver, and a Docker Compose provider also remain future options.

### Local launch specification

The durable local backend produces a backend-neutral, local-shaped internal launch
specification containing:

- immutable agent ID and stable Nostr identity;
- version-pinned harness and runtime executables or packages;
- executable and arguments for `buzz-acp` plus its ACP runtime;
- controlled environment and opaque secret references;
- working directory and persistent workspace/runtime-state paths;
- process group, restart, resource, and health policy.

Container image, mount, network, and orchestration fields are future backend or
supervisor extensions rather than MVP launch-contract requirements.

Model API provider configuration and credentials are runtime inputs represented by
validated configuration and opaque secret references; they do not select the
deployment backend.

### Headless process supervisor

The initial behavioral interface is:

```text
apply(LocalLaunchSpec) -> ProcessReceipt
inspect(ProcessReceipt) -> ObservedState
start(ProcessReceipt)
stop(ProcessReceipt)
delete(ProcessReceipt, RetentionPolicy)
logs(ProcessReceipt, Cursor)
```

The supervisor spawns the child in its own process group, captures bounded and
redacted logs, terminates the group on stop, and applies restart and health policy.
The signer remains a separate process and does not become a general command
runner.

Future supervisor drivers may include Docker Compose, Docker Engine, systemd,
Kubernetes, and VM/job services. The interface should model capabilities rather
than promise that every driver supports identical behavior. Generated container
or orchestration configuration remains output, never the source of truth, and
must not embed secrets.

### Runtime catalog

Buzz Server should ultimately share or generate its runtime catalog from Buzz's
canonical definitions. “Supports Desktop runtimes” means compatible command,
argument, configuration, and packaging semantics; it does not imply one artifact
contains every runtime.

The local-process MVP supports the existing Codex deployment first. Harness and
runtime executables, packages, and adapters must be version-pinned. Additional
runtimes follow after the catalog sharing strategy is decided.

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
GET    /v1/runtimes
GET    /v1/operations/{id}
POST   /v1/agent-drafts
GET    /v1/agent-drafts/{id}
POST   /v1/agent-drafts/{id}/deploy
```

Every agent and draft request names a `community_config_id`; the server never
relies on a process-global active community. The API exposes configurations as
`/v1/communities/{id}`. The relay URL—not this local ID—selects the shared
community on the Buzz/Nostr connection.

Ordinary callers receive product-level controls. Arbitrary environment variables,
mounts, signing operations, and raw supervisor access are privileged administration.

Future bridges consume the API and Buzz events; they do not belong inside agent
lifecycle logic.

## Deployment topology

General topology:

```text
Relay: any reachable host
Execution host: Buzz Server, signer, and supervised local agent processes
```

For self-hosters, installing Buzz Server alongside the relay is conceptually simplest, but co-location is optional and never changes the protocol boundary. Buzz Server and the signer should run as
separate hardened system services. Co-location is only a topology choice. Multiple
communities may run concurrently, and each agent receives only its community's
relay URL and authorization. Process groups, secret paths, working directories,
workspace paths, and runtime state are isolated per agent and connection.

## Readiness and deletion

`running` requires more than a live child process: the expected agent public key,
valid owner authorization, successful connection to the configured relay, a
healthy `buzz-acp` process, and a successful harness-level probe must all agree.

Delete stops the agent immediately and enters recoverable retention. Secrets and
workspace are purged only after the configured retention policy expires. The
default retention period is 30 days, configurable per installation. A daily idempotent purge job retries failures, and an administrator may request immediate purge.
