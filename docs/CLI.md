# `buzz-server` command reference

`buzz-server` is the single operator-facing CLI. Buzz Server owns host, community,
hosted-agent, and secret management. It also bundles the pinned upstream Buzz CLI
and exposes selected Buzz operations through the same namespace.

```text
buzz-server health|status|start|stop|restart|backup|restore|rollback|rotate-owner
buzz-server communities COMMAND [OPTIONS]
buzz-server agents COMMAND [OPTIONS]
buzz-server channels COMMAND --community ID [OPTIONS]
buzz-server secrets COMMAND [OPTIONS]
```

The systemd service uses `buzz-server run --config /etc/buzz-server/config.json`.
Lifecycle commands connect to `/run/buzz-server/lifecycle.sock` by default.

## Communities

### `add`

```sh
sudo buzz-server communities add \
  --display-name 'Engineering' \
  --relay-url 'wss://relay.example.com/'
```

Creates a durable community configuration and returns its generated `community_...`
ID. Relay URLs must use `ws://` or `wss://`.

### `get`

```sh
sudo buzz-server communities get --community community_...
```

### `list`

```sh
sudo buzz-server communities list
```

### `remove`

```sh
sudo buzz-server communities remove --community community_...
```

Removal fails with a conflict while hosted agents still reference the community.

## Hosted agent lifecycle

Mutating hosted-agent commands require:

- `--idempotency KEY`: stable retry key scoped to the caller and operation kind;
- `--correlation ID`: operator-provided trace identifier copied into the durable
  operation and audit record.

The underlying control plane remains asynchronous and durable. The CLI hides the
normal polling step: it waits up to 120 seconds for the operation to reach a
terminal state. On success, commands that leave an agent resource return the final
agent resource. `purge` returns its terminal operation because the agent no longer
exists. `agents operation` remains available for explicit inspection or recovery.

### `create`

```sh
sudo buzz-server agents create \
  --community community_... \
  --display-name 'Build agent' \
  --system-prompt 'Build and verify requested changes.' \
  --runtime codex-acp \
  --idempotency create-build-agent-1 \
  --correlation ticket-1842
```

### `get` / `list`

```sh
sudo buzz-server agents get --agent agent_...
sudo buzz-server agents list
sudo buzz-server agents list --community community_...
```

### `update`

```sh
sudo buzz-server agents update \
  --agent agent_... \
  --display-name 'Primary build agent' \
  --idempotency rename-agent-1 \
  --correlation ticket-1901
```

Include at least one of `--display-name`, `--system-prompt`, or `--runtime`.

### `enable` / `disable`

```sh
sudo buzz-server agents enable --agent agent_... --idempotency enable-1 --correlation maintenance
sudo buzz-server agents disable --agent agent_... --idempotency disable-1 --correlation maintenance
```

### `logs`

```sh
sudo buzz-server agents logs --agent agent_...
sudo buzz-server agents logs --agent agent_... --after 42 --limit 250
```

The default limit is 100; valid values are 1 through 1000.

### `delete` / `purge`

```sh
sudo buzz-server agents delete --agent agent_... --idempotency delete-1 --correlation retire
sudo buzz-server agents purge --agent agent_... --idempotency purge-1 --correlation retire
```

Delete is recoverable until the configured retention deadline. Purge is irreversible
after its operation succeeds.

### `operation`

```sh
sudo buzz-server agents operation --operation operation_...
```

Use this when a synchronous CLI wait times out or when explicitly inspecting the
durable control-plane operation.

### `pubkey`

```sh
sudo buzz-server agents pubkey --agent agent_...
```

Prints the agent's 64-character lowercase Nostr public key by deriving it from the
root-only custodied identity without printing the private key.

## Buzz agent identity operations

Buzz Server's `agents` namespace also includes the compatible upstream Buzz identity
commands `archive`, `unarchive`, and `archived`. These execute against the selected
community with the Buzz Server owner identity:

```sh
sudo buzz-server agents archive --community community_... <PUBKEY> --reason retired
sudo buzz-server agents unarchive --community community_... <PUBKEY> --reason returned
sudo buzz-server agents archived --community community_...
```

The upstream `draft-create` and `draft-update` protocol is also exposed instead of
Buzz Server maintaining a second public draft protocol. These commands are intended
for an already-authorized requesting agent and therefore require that caller's
`BUZZ_PRIVATE_KEY` and `BUZZ_AUTH_TAG`; Buzz Server supplies the selected relay URL:

```sh
buzz-server agents draft-create \
  --community community_... \
  --channel <channel-uuid> \
  --display-name 'Build agent' \
  --system-prompt 'Build and verify requested changes.'
```

The request is sent through Buzz to the owner for review in Buzz Desktop. Nothing is
created until the owner saves it.

## Channels

Buzz Server bundles the `buzz` CLI from the exact Buzz revision pinned by this
release and exposes its `channels` namespace. The wrapper selects a durable Buzz
Server community and supplies the root-only owner identity to the child process.
The bundled binary is internal; no separate public `buzz` executable is installed.

```sh
sudo buzz-server channels list --community community_...
sudo buzz-server channels create --community community_... --name general --type stream --visibility open
sudo buzz-server channels add-member --community community_... --channel <uuid> --pubkey <hex> --role bot
```

Available upstream channel subcommands at the pinned revision are `list`, `get`,
`search`, `create`, `update`, `topic`, `purpose`, `join`, `leave`, `archive`,
`unarchive`, `delete`, `members`, `add-member`, `remove-member`, and
`set-add-policy`.

## Secrets

The former singular `secret` namespace is now plural to match the resource naming
used by the Buzz CLI:

```text
buzz-server secrets encrypt
buzz-server secrets decrypt
buzz-server secrets fingerprint
buzz-server secrets encrypt-passphrase
buzz-server secrets decrypt-passphrase
buzz-server secrets export-nip49
buzz-server secrets import-nip49
buzz-server secrets persist
buzz-server secrets materialize
buzz-server secrets clear-local
```

Run `buzz-server secrets <command> --help` for exact arguments.

## Output and exit status

Lifecycle success responses are compact JSON. API errors are JSON and use exit
status `1`; usage, parsing, connection, framing, or response-decoding failures use
exit status `2`. Upstream Buzz commands preserve the bundled Buzz CLI's JSON output
and exit status behavior.

See [Lifecycle API](LIFECYCLE_API.md) for the underlying request, resource,
authentication, retention, and error semantics.
