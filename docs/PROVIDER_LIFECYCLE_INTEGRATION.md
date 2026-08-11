# Provider lifecycle integration

The provider host and durable deployment coordinator deliberately do not run
inside `LifecycleApplication::create_agent`. Create records intent before the
signer has produced the deployment authorization; invoking a provider there
would invert the required signer-before-provider ordering.

Provider protocol compatibility is complete. To make an external provider a
selectable durable lifecycle backend, the provider lifecycle adapter must:

1. add a durable backend selection to agent intent:
   `Local` or `Provider { id, config }`; provider config is validated against
   the negotiated descriptor and contains no secret-shaped fields;
2. expose discovered, negotiated `ProviderDescriptor` values (including
   `config_schema`) through an owner-authorized private read route;
3. let create/update persist backend intent and an operation without invoking
   provider code;
4. in the operation worker, complete signer authorization first, discover the
   explicitly trusted provider ID/hash, negotiate `info`, and only then pass an
   already-signed Desktop-compatible payload closure to
   `ProviderDeploymentCoordinator::deploy_once`;
5. implement `ProviderDeploymentRepository::begin` as an atomic
   insert-if-absent and `complete` as a compare-and-set from the exact in-flight
   record; use the durable operation ID as `request_id`;
6. on restart, return completed receipts without rebuilding secrets. For an
   in-flight record, reconcile external state by stable request ID and provider
   identity before completing it; never issue a blind second deploy;
7. check negotiated lifecycle support before changing desired state. An
   unsupported action returns the stable API unsupported error and leaves
   agent intent, operation state, and audit history unchanged;
8. never grant a provider the Docker socket or a general-purpose helper. The
   service passes only explicitly configured non-secret environment variables,
   and retains the host's timeout, input/output cap, private staging, process
   group, and systemd filesystem/UID restrictions.

This integration stays deliberately narrow: provider hosting remains separate
from HTTP/Tauri code, and protocol-v1 deploy is not treated as a complete lifecycle
backend.
