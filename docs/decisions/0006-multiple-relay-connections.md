# ADR 0006: Isolate multiple relay connections as client workspaces

Status: accepted

Buzz Server supports multiple explicitly configured relay connections, matching
Buzz Desktop's multiple-community behavior. Each connection binds a relay URL to
an owner identity/key reference and has an immutable internal `connection_id`.

Agents, drafts, operations, credentials, provider configuration, workspaces,
caches, jobs, logs, and runtime state are connection-scoped. There is no global
active relay and no implicit cross-connection data flow. Cross-relay automation
requires a separately authorized and audited bridge.

This is client-side multi-workspace support, not a multi-tenant Buzz Server data
model. Community tenancy and host routing remain relay implementation details.
