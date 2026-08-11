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

Do not include a Docker Compose provider in the initial provider-compatibility
implementation. Keep Compose as a possible external deployment provider, but defer
it until its lifecycle and secret-delivery behavior can be defined without creating
an incompatible provider contract or persisting agent credentials in generated
Compose configuration.

The existing Buzz provider protocol and Kubernetes fixtures remain the compatibility
target in the meantime. Compose can be added later through the same external-provider
boundary once the missing lifecycle and secret-delivery pieces are defined.

## Consequences

Provider hosting stays independent of the built-in local supervisor. Deferring
Compose avoids presenting process creation as lifecycle support and avoids a
new plaintext credential surface. This decision can be revisited without a
wire break when the capability and secret-delivery prerequisites exist.
