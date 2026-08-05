# `buzz-agentctl` command reference

`buzz-agentctl` is the machine-readable same-host client for the Buzz Server
lifecycle API. It connects to the Unix socket and prints exactly one compact JSON
response on stdout.

```text
buzz-agentctl [--socket PATH] COMMAND [OPTIONS]
```

The default socket is `/run/buzz-server/lifecycle.sock`.

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
buzz-agentctl create \
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
buzz-agentctl get --agent agent_...
```

### `list`

```sh
buzz-agentctl list
buzz-agentctl list --community community_...
```

### `update`

```sh
buzz-agentctl update \
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
buzz-agentctl enable \
  --agent agent_... \
  --idempotency enable-agent-1 \
  --correlation maintenance-1
```

### `disable`

```sh
buzz-agentctl disable \
  --agent agent_... \
  --idempotency disable-agent-1 \
  --correlation maintenance-1
```

### `logs`

```sh
buzz-agentctl logs --agent agent_...
buzz-agentctl logs --agent agent_... --limit 250
buzz-agentctl logs --agent agent_... --after 42 --limit 250
```

The default limit is 100; valid values are 1 through 1000. Continue from the
returned `next_cursor` with `--after`.

### `delete`

```sh
buzz-agentctl delete \
  --agent agent_... \
  --idempotency delete-agent-1 \
  --correlation retire-1
```

Delete is recoverable until the configured retention deadline. It stops the
agent and records deleted intent without immediately removing all state.

### `purge`

```sh
buzz-agentctl purge \
  --agent agent_... \
  --idempotency purge-agent-1 \
  --correlation retire-1
```

Purge is immediate and irreversible after the operation succeeds.

### `operation`

```sh
buzz-agentctl operation --operation operation_...
```

Typical polling loop:

```sh
while :; do
  response=$(buzz-agentctl operation --operation operation_...) || exit $?
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
buzz-agentctl draft-submit \
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
buzz-agentctl draft-get --draft draft_...
```

Draft submitters may read only drafts owned by their authenticated Unix UID.

### `draft-promote`

```sh
buzz-agentctl draft-promote \
  --draft draft_... \
  --idempotency promote-draft-1 \
  --correlation approval-92
```

Promotion is administrator-only and returns the same operation shape as `create`.

## Custom socket

Place `--socket` before the command:

```sh
buzz-agentctl --socket /custom/run/lifecycle.sock list
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
