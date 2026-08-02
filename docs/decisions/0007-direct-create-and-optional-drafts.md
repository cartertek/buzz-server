# ADR 0007: Direct creation is primary and drafts are optional

Status: accepted

An authorized `POST /v1/agents` immediately begins the idempotent authorization
and deployment operation. It does not require an intermediate review state.

Agent drafts are optional, non-secret review resources for conversational agents
and lower-trust automation. A draft has no agent keypair, owner authorization,
workspace, provider deployment, or running service. Explicit approval validates
the current proposal and invokes the same direct-create operation.

Drafts are not agent lifecycle or reconciliation states. Providers and
supervisors receive only fully authorized deployments.
