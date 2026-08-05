# ADR 0013: Defer a Docker Compose provider

Status: accepted

## Context

Milestone 4 evaluates Docker Compose as an optional external deployment path.
Provider protocol v1 gives a provider the complete authorized deploy payload,
including the agent private key, but defines only `info` and `deploy`. Compose
can create a container, but durable lifecycle reconciliation, log ownership,
secret rotation, deletion, and readiness remain server responsibilities.

The pinned Buzz revision contains a maintained Kubernetes reference provider,
an immutable-image policy, and a complete golden wire corpus. It contains no
Compose provider, Compose fixtures, or provider-protocol library crate. A
Compose implementation here would therefore introduce a second, unshared
contract and would need either plaintext secrets in generated YAML/environment
or a new secret-injection mechanism that protocol v1 does not specify.

## Decision

Do not include a Docker Compose provider in Milestone 4. Keep Compose eligible
as a separately installed `buzz-backend-*` provider after it has:

- a non-persistent secret delivery design (generated Compose YAML must never
  contain agent credentials);
- versioned lifecycle capabilities or an explicit server reconciliation
  adapter;
- immutable image and provider-binary trust policy;
- golden wire and end-to-end readiness fixtures shared with Buzz.

Milestone 4 instead proves the existing provider-v1 contract with the upstream
Kubernetes fixture corpus and a deterministic fake provider.

## Consequences

Provider hosting stays independent of the built-in local supervisor. Deferring
Compose avoids presenting process creation as lifecycle support and avoids a
new plaintext credential surface. This decision can be revisited without a
wire break when the capability and secret-delivery prerequisites exist.
