# Provider compatibility

Buzz Server hosts version-1 external providers through an administrator trust
boundary:

1. discover only executable `buzz-backend-*` files in configured directories;
2. require explicit trust over provider ID, canonical path, and SHA-256;
3. copy the trusted bytes into a private staging directory and verify the hash;
4. invoke `info` with an empty ambient environment plus only explicitly
   configured, non-secret variables;
5. validate provider identity, protocol version, and the supported scalar
   configuration-schema subset;
6. build the secret-bearing deploy payload only after successful negotiation;
7. invoke `deploy` on the same immutable staged bytes.

Provider protocol v1 is deploy-only. Inspect, logs, enable, disable, and delete
are explicit unsupported provider actions; server lifecycle code must not
silently forward them to a provider.

An optional `capabilities` block is strictly versioned independently from the
deploy protocol. Absent capabilities mean deploy-only; lifecycle actions
without a lifecycle protocol version, a mismatched version, or an attempt to
re-advertise deploy are rejected during `info` negotiation. This keeps future
lifecycle evolution explicit rather than assigning new meaning to v1.

`ProviderDeploymentCoordinator` is the narrow durable handoff for the
post-authorization operation worker. It uses the durable operation ID as the
provider request ID and records the exact provider ID/hash/external agent ID.
On restart an existing receipt is returned without reconstructing secrets or
reinvoking the provider. The application still owns desired state and must
leave it unchanged when an unsupported lifecycle action is reported.

## Compatibility evidence

`tests/fixtures/provider-wire/` is the complete provider-wire corpus from the
pinned Buzz revision. `tests/provider_compat.rs` has a directory completeness
guard and drives both a deterministic fake provider and the Kubernetes
reference contract through real stdin/stdout subprocess calls. The adversarial
fake records its `info` environment and proves that ambient server credentials
and the deferred deploy payload are absent before negotiation.

The pinned Buzz workspace has no reusable provider-protocol library crate. Its
Kubernetes provider is a binary crate and declares its own wire types, with the
golden fixtures as the cross-project arbiter. Buzz Server therefore consumes
the fixtures rather than creating a permanent private protocol fork.

Docker Compose is intentionally deferred; see
[`decisions/0013-defer-compose-provider.md`](decisions/0013-defer-compose-provider.md).
