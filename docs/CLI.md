# `buzz-server` command reference

`buzz-server` is the single operator-facing CLI. Server/service operations are
top-level commands, agent lifecycle operations live under `agent`, and key
management operations live under `secrets`.

```text
buzz-server health|status|start|stop|restart|backup|restore|rollback|rotate-owner
buzz-server agent [--socket PATH] COMMAND [OPTIONS]
buzz-server secret COMMAND [OPTIONS]
```

The systemd service uses `buzz-server run --config /etc/buzz-server/config.json`.
The agent client connects to `/run/buzz-server/lifecycle.sock` by default and
prints exactly one compact JSON response on stdout.

## Exit status

- `0`: the server returned a successful API response;
- `1`: the server returned a structured API error;
- `2`: usage, parsing, connection, framing, or response-decoding failure.

Structured API responses, including status and resource data, go to stdout.
Usage and transport failures go to stderr.

## Shared mutating options

Mutating commands require:

- `--idempotency KEY`: stable retry key scoped to the caller and operation kind;
- `--correlation ID`: operator-provided trace identifier copied into the durable
  operation and audit record.

A safe retry repeats the same command with the same idempotency key. Reusing the
key for a different request returns a conflict.

## Commands

### `create`

```sh
buzz-server agent create \
  --community community_... \
  --display-name 'Build agent' \
  --system-prompt 'Build and verify requested changes.' \
  --runtime codex-acp \
  --idempotency create-build-agent-1 \
  --correlation ticket-1842
```

Returns a durable operation. Poll it with `operation`.

### `get`

```sh
buzz-server agent get --agent agent_...
```

### `list`

```sh
buzz-server agent list
buzz-server agent list --community community_...
```

### `update`

```sh
buzz-server agent update \
  --agent agent_... \
  --display-name 'Primary build agent' \
  --idempotency rename-agent-1 \
  --correlation ticket-1901
```

Include at least one of:

- `--display-name NAME`
- `--system-prompt PROMPT`
- `--runtime RUNTIME_ID`

### `enable`

```sh
buzz-server agent enable \
  --agent agent_... \
  --idempotency enable-agent-1 \
  --correlation maintenance-1
```

### `disable`

```sh
buzz-server agent disable \
  --agent agent_... \
  --idempotency disable-agent-1 \
  --correlation maintenance-1
```

### `logs`

```sh
buzz-server agent logs --agent agent_...
buzz-server agent logs --agent agent_... --limit 250
buzz-server agent logs --agent agent_... --after 42 --limit 250
```

The default limit is 100; valid values are 1 through 1000. Continue from the
returned `next_cursor` with `--after`.

### `delete`

```sh
buzz-server agent delete \
  --agent agent_... \
  --idempotency delete-agent-1 \
  --correlation retire-1
```

Delete is recoverable until the configured retention deadline. It stops the
agent and records deleted intent without immediately removing all state.

### `purge`

```sh
buzz-server agent purge \
  --agent agent_... \
  --idempotency purge-agent-1 \
  --correlation retire-1
```

Purge is immediate and irreversible after the operation succeeds.

### `operation`

```sh
buzz-server agent operation --operation operation_...
```

### `pubkey`

```sh
sudo buzz-server agent pubkey --agent agent_...
```

Prints the agent's 64-character lowercase Nostr public key. The command derives
the public key from the root-only custodied identity and never prints the private
key. This is useful when adding a Server-managed agent to a Buzz channel.

Typical polling loop:

```sh
while :; do
  response=$(buzz-server agent operation --operation operation_...) || exit $?
  printf '%s\n' "$response"
  status=$(printf '%s' "$response" | jq -r '.value.value.status')
  case "$status" in
    succeeded) break ;;
    failed|cancelled) exit 1 ;;
  esac
  sleep 1
done
```

### `draft-submit`

```sh
buzz-server agent draft-submit \
  --community community_... \
  --display-name 'Build agent' \
  --system-prompt 'Build and verify requested changes.' \
  --runtime codex-acp \
  --idempotency draft-build-agent-1 \
  --correlation request-92
```

This creates a non-secret review resource. It does not mint an agent identity or
start a deployment.

### `draft-get`

```sh
buzz-server agent draft-get --draft draft_...
```

Draft submitters may read only drafts owned by their authenticated Unix UID.

### `draft-promote`

```sh
buzz-server agent draft-promote \
  --draft draft_... \
  --idempotency promote-draft-1 \
  --correlation approval-92
```

Promotion is administrator-only and returns the same operation shape as `create`.

## Custom socket

Place `--socket` before the command:

```sh
buzz-server agent --socket /custom/run/lifecycle.sock list
```

The caller's UID must appear in `administrator_uids` or
`draft_submitter_uids`; socket accessibility alone does not authorize the call.

## Output processing

Successful output is a compact JSON envelope:

```json
{"status":"ok","value":{"resource":"agents","value":[]}}
```

API failures are also JSON and cause exit status 1:

```json
{"status":"error","value":{"code":"not_found","message":"resource not found"}}
```

See [Lifecycle API](LIFECYCLE_API.md) for complete request, resource,
authentication, retention, and error semantics.
