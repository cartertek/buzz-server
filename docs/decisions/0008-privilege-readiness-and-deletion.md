# ADR 0008: Bound supervisor privilege and verify operational readiness

Status: accepted

The main API daemon does not receive unrestricted Docker access. A separate thin
privileged helper exposes only the supervisor operations required by the driver;
it is not a general command runner. The owner signer remains isolated from both.

An agent is ready only when its expected public key and owner authorization are
valid, it has connected to the configured relay, `buzz-acp` is healthy, and a
harness-level probe succeeds. Container-running status alone is insufficient.

Delete stops the service immediately and retains secrets and workspace
recoverably until a configurable retention policy expires, after which purge is
performed. The default duration is intentionally still open.
