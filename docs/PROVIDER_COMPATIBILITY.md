# Provider compatibility

Buzz Server hosts version-1 external providers through an administrator trust
boundary:

1. discover only executable `buzz-backend-*` files in configured directories;
2. require explicit trust over provider ID, canonical path, and SHA-256;
3. copy the trusted bytes into a size-capped, sealed Linux memory file and
   verify the hash before making that immutable descriptor executable;
4. invoke `info` with an empty ambient environment plus only explicitly
   configured, non-secret variables;
5. validate provider identity, protocol version, and the supported scalar
   configuration-schema subset;
6. build the secret-bearing deploy payload only after successful negotiation;
7. invoke `deploy` on the same immutable staged bytes.

Provider protocol v1 defines deploy only. Other operations are forwarded only
when the provider advertises them under the independently versioned lifecycle
capability; unsupported operations never invoke the provider.

An optional `capabilities` block is strictly versioned independently from the
deploy protocol. Absent capabilities mean deploy-only; lifecycle actions
without a lifecycle protocol version, a mismatched version, or an attempt to
re-advertise deploy are rejected during `info` negotiation. This keeps future
lifecycle evolution explicit rather than assigning new meaning to v1.

`ProviderDeploymentCoordinator` is the narrow durable handoff for the
post-authorization operation worker. It uses the durable operation ID as the
provider request ID and atomically records an in-flight intent before external
invocation. Concurrent callers cannot both deploy. A completed receipt returns
without reconstructing secrets or reinvoking the provider. A crash between
external success and receipt completion remains explicitly in-flight and must
be reconciled by stable request ID/provider identity; it is never blindly
redeployed. Providers must therefore guarantee stable-ID convergence or offer
a provider-specific reconciliation implementation. The application still owns
desired state and must leave it unchanged for unsupported lifecycle actions.

## Compatibility evidence

`tests/fixtures/provider-wire/` is the complete provider-wire corpus from the
pinned Buzz revision. `tests/provider_compat.rs` has a directory completeness
guard and drives both a deterministic fake provider and the Kubernetes
reference contract through deterministic shell subprocesses. Adversarial tests
cover stdin backpressure, forked pipe holders, sealed-binary self-replacement,
ambient credential removal, and the durable crash window. The CI helper also
builds the actual pinned Kubernetes provider and runs its own upstream
`wire_fixtures` test target; it does not claim to invoke that binary through
the Server host test.

Explicit environment configuration is an administrator allowlist assertion,
not proof that a value is harmless. Secret-shaped names are rejected and the
ambient environment is cleared, while deployment policy must still restrict
who can configure the remaining names and values.

The pinned Buzz workspace has no reusable provider-protocol library crate. Its
Kubernetes provider is a binary crate and declares its own wire types, with the
golden fixtures as the cross-project arbiter. Buzz Server therefore consumes
the fixtures rather than creating a permanent private protocol fork.

Docker Compose is intentionally deferred; see
[`decisions/0013-defer-compose-provider.md`](decisions/0013-defer-compose-provider.md).
