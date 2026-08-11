# ADR 0013: Defer a Docker Compose provider

Status: accepted

## Context

The provider-compatibility work evaluated Docker Compose as an optional external
deployment path.
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

Do not implement a Buzz Server-specific Docker Compose provider. The existing Buzz
provider protocol and Kubernetes fixtures define the compatibility target; adding a
Compose provider here would create a separate provider contract and a new secret
delivery mechanism that Buzz itself does not define.

If Buzz gains a Compose provider or the provider protocol later defines the lifecycle
and secret-delivery behavior Compose needs, Buzz Server can support it through the
same external-provider compatibility boundary.

## Consequences

Provider hosting stays independent of the built-in local supervisor. Deferring
Compose avoids presenting process creation as lifecycle support and avoids a
new plaintext credential surface. This decision can be revisited without a
wire break when the capability and secret-delivery prerequisites exist.
