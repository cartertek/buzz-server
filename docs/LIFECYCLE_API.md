# Lifecycle API

Buzz Server exposes one transport-independent lifecycle API through two private
adapters:

- a Unix domain socket authenticated with kernel peer credentials; and
- HTTPS authenticated with NIP-98 signed requests.

Both transports accept the same JSON request body and return the same JSON
response envelope.

## Authority classes

Configured callers receive one fixed authority:

- `administrator`: may manage communities, read and operate agents, purge retained agents, and
  promote drafts;
- `draft_submitter`: may submit drafts and read only drafts owned by the same
  Unix UID or Nostr public key.

Clients cannot include an authority in request JSON. The transport derives the
authenticated principal before decoding the lifecycle request.

## Response envelope

Successful responses use:

```json
{
  "status": "ok",
  "value": {
    "resource": "operation",
    "value": {}
  }
}
```

The `resource` value is one of `community`, `communities`, `agent`, `agents`,
`logs`, `operation`, or `draft`. The nested `value` is the corresponding resource documented below.

Application errors use:

```json
{
  "status": "error",
  "value": {
    "code": "invalid_request",
    "message": "request is invalid",
    "field": "display_name"
  }
}
```

`field` is present only for field-specific validation errors. Stable error codes
are `invalid_request`, `unauthorized`, `forbidden`, `not_found`, `conflict`,
`unsupported`, and `internal`.

The HTTPS adapter maps these to HTTP 400, 401, 403, 404, 409, 501, and 500.
Unix-socket clients receive the JSON envelope directly.

## Command metadata

Durable agent mutations contain:

```json
{
  "idempotency_key": "create-build-agent-1",
  "correlation_id": "ticket-1842"
}
```

An idempotency key is scoped to the authenticated principal and operation kind.
Reusing it with the same request returns the existing durable operation. Reusing
it with a different request returns `conflict`. Correlation IDs are copied into
operations and audit records for tracing.

## Requests

Requests are tagged JSON objects with `route` and `request` fields.

### Add a community

```json
{
  "route": "add_community",
  "request": {
    "display_name": "Engineering",
    "relay_url": "wss://relay.example.com/"
  }
}
```

Returns a `community` resource with a generated `community_...` ID. Community
configuration changes are synchronous and administrator-only.

### Update, get, list, or remove communities

```json
{"route":"update_community","request":{"community_id":"community_...","display_name":"Platform Engineering"}}
{"route":"get_community","request":{"community_id":"community_..."}}
{"route":"list_communities"}
{"route":"remove_community","request":{"community_id":"community_..."}}
```

Community display names are local server labels. Removal returns `conflict` while any enabled or disabled agent still references the community, or while a deleted agent has not completed shutdown. If every remaining agent is successfully deleted, removal purges those retained deleted-agent records before removing the community. Community state is stored only in the Buzz Server state database.

### Await an operation

```json
{"route":"await_operation","request":{"operation_id":"operation_..."}}
```

The request returns immediately if the operation is already terminal. Otherwise the
server waits for an in-process completion notification and then returns the terminal
operation. If the daemon exits, the connection closes so clients can report the transport
failure instead of silently waiting on stale state.

### Create an agent

```json
{
  "route": "create_agent",
  "request": {
    "metadata": {
      "idempotency_key": "create-build-agent-1",
      "correlation_id": "ticket-1842"
    },
    "agent": {
      "community_config_id": "community_...",
      "display_name": "Build agent",
      "system_prompt": "Build and verify requested changes.",
      "runtime_id": "codex-acp"
    }
  }
}
```

Returns an `operation` resource. Creation is durable and asynchronous. Clients that want
synchronous command behavior should send one `await_operation` request for the returned
operation ID; the server holds that request until the operation becomes terminal or the
server-side wait window expires. Repeated polling is not required.

### Get an agent

```json
{
  "route": "get_agent",
  "request": {
    "agent_id": "agent_..."
  }
}
```

### List agents

```json
{
  "route": "list_agents",
  "request": {
    "community_config_id": null,
    "include_deleted": false
  }
}
```

Set `community_config_id` to restrict results to one configured community. Recoverably deleted agents are excluded by default; set `include_deleted` to `true` to include them.

### Update an agent

```json
{
  "route": "update_agent",
  "request": {
    "metadata": {
      "idempotency_key": "rename-agent-1",
      "correlation_id": "ticket-1901"
    },
    "agent_id": "agent_...",
    "changes": {
      "display_name": "Primary build agent",
      "system_prompt": null,
      "runtime_id": null
    }
  }
}
```

At least one change must be non-null.

### Enable or disable an agent

```json
{
  "route": "change_agent_state",
  "request": {
    "metadata": {
      "idempotency_key": "disable-agent-1",
      "correlation_id": "maintenance-1"
    },
    "agent_id": "agent_...",
    "desired_state": "disabled"
  }
}
```

`desired_state` may be `enabled` or `disabled`. Deleted state must use the delete
operation.

### Read agent logs

```json
{
  "route": "agent_logs",
  "request": {
    "agent_id": "agent_...",
    "after_cursor": null,
    "limit": 100
  }
}
```

`limit` must be between 1 and 1000. Use the returned `next_cursor` as
`after_cursor` to continue reading.

### Recoverable delete

```json
{
  "route": "delete_agent",
  "request": {
    "metadata": {
      "idempotency_key": "delete-agent-1",
      "correlation_id": "retire-1"
    },
    "agent_id": "agent_..."
  }
}
```

Delete stops the deployment and records deleted intent while retaining the agent
until its configured `purge_after` deadline. Re-enabling before purge recovers the
agent.

### Immediate purge

```json
{
  "route": "purge_agent",
  "request": {
    "metadata": {
      "idempotency_key": "purge-agent-1",
      "correlation_id": "retire-1"
    },
    "agent_id": "agent_..."
  }
}
```

Purge stops the deployment and atomically removes retained state after successful
reconciliation. Purged agent IDs are tombstoned and are not recreated by stale
operations.

### Poll an operation

```json
{
  "route": "get_operation",
  "request": {
    "operation_id": "operation_..."
  }
}
```

### Submit a draft

```json
{
  "route": "submit_draft",
  "request": {
    "metadata": {
      "idempotency_key": "draft-build-agent-1",
      "correlation_id": "request-92"
    },
    "agent": {
      "community_config_id": "community_...",
      "display_name": "Build agent",
      "system_prompt": "Build and verify requested changes.",
      "runtime_id": "codex-acp"
    }
  }
}
```

Draft submission does not mint an identity or deploy an agent.

### Get a draft

```json
{
  "route": "get_draft",
  "request": {
    "draft_id": "draft_..."
  }
}
```

Draft submitters may read only their own drafts. Administrators may read any
draft.

### Promote a draft

```json
{
  "route": "promote_draft",
  "request": {
    "metadata": {
      "idempotency_key": "promote-draft-1",
      "correlation_id": "approval-92"
    },
    "draft_id": "draft_..."
  }
}
```

Promotion is administrator-only and enters the same direct-create application
path as a normal create request.

## Resources

### Agent

```json
{
  "id": "agent_...",
  "community_config_id": "community_...",
  "display_name": "Build agent",
  "system_prompt": "Build and verify requested changes.",
  "runtime_id": "codex-acp",
  "desired_state": "enabled",
  "purge_after": null
}
```

### Operation

```json
{
  "id": "operation_...",
  "kind": "create_agent",
  "status": "pending",
  "agent_id": "agent_...",
  "correlation_id": "ticket-1842",
  "error_code": null,
  "created_at": 1785890000,
  "updated_at": 1785890000
}
```

Operation status is `pending`, `running`, `succeeded`, `failed`, or `cancelled`.

### Logs

```json
{
  "entries": [
    {
      "cursor": "42",
      "occurred_at": 1785890000,
      "stream": "stderr",
      "redacted_message": "agent ready"
    }
  ],
  "next_cursor": "42"
}
```

### Draft

```json
{
  "id": "draft_...",
  "owner": {
    "kind": "unix_uid",
    "uid": 1000
  },
  "agent": {
    "community_config_id": "community_...",
    "display_name": "Build agent",
    "system_prompt": "Build and verify requested changes.",
    "runtime_id": "codex-acp"
  }
}
```

NIP-98-owned drafts use `{"kind":"nostr_pubkey","pubkey":"..."}`.

## Unix socket transport

The default socket is `/run/buzz-server/lifecycle.sock`. It may be changed with
`lifecycle_api.unix_socket`.

Each request and response is one unsigned, big-endian 32-bit length followed by
that many JSON bytes. Requests and responses are limited to 1 MiB. Connections
have a 10-second I/O timeout.

The server authenticates the connection with `SO_PEERCRED`. UIDs are mapped by
`lifecycle_api.administrator_uids` and `lifecycle_api.draft_submitter_uids`.
Filesystem ownership is not accepted as identity, and an unlisted UID is rejected.
The supplied `buzz-server agents` client implements this framing.

## HTTPS and NIP-98 transport

TLS is enabled by configuring `lifecycle_api.tls` with a listen address,
certificate, private key, canonical origin, authorized public keys, and freshness
window. This is ordinary server-authenticated HTTPS with NIP-98 client
authentication; it is not mutual TLS.

The HTTP method and path are transport selectors only. The lifecycle operation is
selected by the JSON `route`. A client may use a stable path such as
`POST /lifecycle`.

The request must include:

```text
Authorization: Nostr <base64-encoded-event-json>
```

The signed event must:

- use Nostr kind 27235;
- contain a `u` tag equal to `canonical_origin + original request URI`;
- contain a `method` tag equal to the exact HTTP method;
- contain a lowercase SHA-256 `payload` tag when the body is non-empty;
- be within the configured freshness window;
- be signed by an allowlisted administrator or draft-submitter public key; and
- not have been used before.

Replay claims are stored durably in SQLite. A malformed, stale, mismatched, or
replayed proof returns HTTP 401. A valid signature from an unconfigured public key
returns HTTP 403.
