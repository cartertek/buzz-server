# ADR 0006: Isolate multiple communities as client workspaces

Status: accepted

Buzz Server supports multiple explicitly configured communities, matching Buzz
Desktop. Each `CommunityConfig` has a Server-local UUID `id`, display name, one
authoritative `relay_url`, and an owner identity/key reference. Foreign keys use
`community_config_id`.

Agents, drafts, operations, credentials, provider configuration, workspaces,
caches, jobs, logs, and runtime state are community-scoped. There is no global
active community and no implicit cross-community data flow. Cross-relay automation
requires a separately authorized and audited bridge.

The relay URL is the shared locator used by multiple clients. The configuration
ID is local control-plane metadata, not the relay's internal database ID and not
a Buzz/Nostr wire field. One physical relay serving multiple communities is
represented by their distinct authoritative URLs. Independent relays are likewise
separate communities. Buzz Server does not merge several relay URLs into one
community; failover or federation requires a future explicit protocol/feature.
This is client-side multi-workspace support, not a multi-tenant Server data model.
