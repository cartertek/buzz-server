# `buzz-server` command reference

`buzz-server` is the single operator-facing CLI. Buzz Server owns host, community,
hosted-agent, and secret management. It also bundles the pinned upstream Buzz CLI
and exposes selected Buzz operations through the same namespace.

```text
buzz-server health|status|start|stop|restart|backup|restore|rollback
buzz-server communities COMMAND [OPTIONS]
buzz-server agents COMMAND [OPTIONS]
buzz-server channels COMMAND --community ID [OPTIONS]
buzz-server secrets COMMAND [OPTIONS]
```

The systemd service uses `buzz-server run --config /etc/buzz-server/config.json`.
Lifecycle commands connect to `/run/buzz-server/lifecycle.sock` by default.

## Communities

### `join`

```sh
sudo buzz-server communities join \
  --display-name 'Engineering' \
  --relay-url 'wss://relay.example.com/'
```

The command prompts for the Nostr private key without echoing it. For automation, pass `--secret-file FILE` instead. Buzz Server derives the public key, verifies the relay using the same NIP-43 membership semantics as Buzz Desktop, securely custodies/deduplicates the secret by pubkey, and persists the community only after verification succeeds. Relay URLs must use `ws://` or `wss://`.

### `get`

```sh
sudo buzz-server communities get --community community_...
```

### `list`

```sh
sudo buzz-server communities list
```

### `update`

```sh
sudo buzz-server communities update \
  --community community_... \
  --display-name 'Platform Engineering'
```

Updates the local display label. The relay URL is unchanged.

### `delete`

```sh
sudo buzz-server communities delete --community community_...
```

Deletion fails with a conflict while any enabled or disabled agent still references the community. If every remaining agent is already successfully deleted, deleting the community also permanently purges those retained deleted-agent records and their local artifacts.

## Personas

Persona configuration is file-backed and can also be managed through a small CLI surface:

```sh
sudo buzz-server personas create --display-name 'Reviewer' --system-prompt 'Review changes carefully.' --runtime codex-acp
sudo buzz-server personas list
sudo buzz-server personas get --persona <persona-id>
sudo buzz-server personas update --persona <persona-id> --display-name 'Senior reviewer'
sudo buzz-server personas delete --persona <persona-id>
```

`create` generates and returns the persona ID. The CLI covers the common fields; edit
`/var/lib/buzz-server/agent-config/personas/<id>.json` directly for the complete definition.
A persona cannot be deleted while retained agent files still reference it; purge those agents first.

## Hosted agent lifecycle

Mutating hosted-agent commands generate idempotency and correlation identifiers
automatically. Scripts may pass `--idempotency KEY` to make retries deterministic
and `--correlation ID` to supply a trace identifier; either option may be omitted.

The underlying control plane remains asynchronous and durable. The CLI uses a single
event-driven completion wait: after submitting a mutation it opens one `await_operation`
request, which the server completes when the durable operation becomes terminal. It does
not poll. On success, commands that leave an agent resource return the final agent
resource. `purge` returns its terminal operation because the agent no longer exists.
`agents operation` remains available for explicit inspection or recovery after a daemon
restart, transport failure, or operator interruption.

### `create`

Create a standalone agent:

```sh
sudo buzz-server agents create \
  --community community_... \
  --display-name 'Build agent' \
  --system-prompt 'Build and verify requested changes.' \
  --runtime codex-acp
```

Or create an agent from a persona stored in
`/var/lib/buzz-server/agent-config/personas/<id>.json`:

```sh
sudo buzz-server agents create \
  --community community_... \
  --display-name 'Reviewer' \
  --persona reviewer
```

`--system-prompt` applies only to standalone agents. Persona-backed agents use the
linked persona's system prompt; update the persona to change it. `--runtime` is required
for a standalone agent and optional for a persona-backed agent, where it acts as an
explicit runtime override.
The resulting agent configuration is written to
`/var/lib/buzz-server/agent-config/agents/<agent-id>.json` and is reloaded when the
service restarts.

For standalone agents, `--system-prompt-file PATH` loads a UTF-8 prompt when the
inline prompt is empty. `PATH` must be an absolute administrator-selected path;
it is read directly and is not reinterpreted relative to the agent-config store.
The file is reread whenever the agent configuration is
resolved, so edits take effect on the next reconcile. Missing, unreadable,
non-regular, and invalid-UTF-8 files fail with an actionable error.
A non-empty inline `--system-prompt` always wins. To transition an existing inline
agent, update it with `--system-prompt-file PATH` alone; this stores an explicit
blank inline prompt and activates file-backed resolution. Supplying a non-empty
inline prompt in the same update keeps that prompt authoritative.

### `get` / `list`

```sh
sudo buzz-server agents get --agent agent_...
sudo buzz-server agents list
sudo buzz-server agents list --community community_...
sudo buzz-server agents list --include-deleted
```

Normal lists exclude recoverably deleted agents. Use `--include-deleted` to inspect
retained deleted agents during their recovery window. `get --agent ...` continues to
address a retained deleted agent directly.

### `update`

```sh
sudo buzz-server agents update \
  --agent agent_... \
  --display-name 'Primary build agent'
```

Include at least one of `--display-name`, `--system-prompt`, or `--runtime`.

### `enable` / `disable` / `reload`

```sh
sudo buzz-server agents enable --agent agent_...
sudo buzz-server agents disable --agent agent_...
sudo buzz-server agents reload --agent agent_...
```

`reload` composes `disable` followed by `enable` for the selected agent. The
enable operation is not attempted if disabling fails.

### `logs`

```sh
sudo buzz-server agents logs --agent agent_...
sudo buzz-server agents logs --agent agent_... --after 42 --limit 250
```

The default limit is 100; valid values are 1 through 1000.

### `delete` / `purge`

```sh
sudo buzz-server agents delete --agent agent_...
sudo buzz-server agents purge --agent agent_...
```

Delete stops the agent and retains its record for recovery until the configured
retention deadline. Repeating delete does not extend that deadline. Recoverably deleted
agents are hidden from normal `list` output. Purge is irreversible after its operation
succeeds.

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
community with that community's associated identity:

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
Server community and supplies that community's root-only associated identity to the child process.
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

## Messages

Buzz Server forwards the pinned upstream message CLI without duplicating its
implementation. Select the community before any upstream options:

```sh
buzz-server messages get --community community_... --channel <uuid>
buzz-server messages send --community community_... --channel <uuid> --content 'hello'
```

The wrapper supplies the selected community relay and owner identity while
preserving upstream arguments, standard streams, and exit status.

## Live events

Subscribe to every event accepted by the selected community relay:

```sh
buzz-server events subscribe --community community_...
```

Output is JSONL. `event`, `eose`, `closed`, `notice`, and `error` objects are
reported as received. EOSE does not end the stream; relay closure or transport
failure reconnects with a fresh unrestricted request. Press Ctrl-C (or send
SIGTERM) to close the connection. Events are ephemeral output only: events are
not replayed or persisted, and only events received while connected are shown.

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
