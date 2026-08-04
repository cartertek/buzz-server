CREATE TABLE agent_drafts (
    id TEXT PRIMARY KEY,
    owner_key TEXT NOT NULL,
    document TEXT NOT NULL,
    promoted_operation_id TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE agent_logs (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_id TEXT NOT NULL REFERENCES agent_specs(id) ON DELETE CASCADE,
    cursor TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    stream TEXT NOT NULL,
    redacted_message TEXT NOT NULL,
    UNIQUE(agent_id, cursor)
);

CREATE INDEX agent_logs_agent_sequence_idx ON agent_logs(agent_id, sequence);

CREATE TABLE nip98_replay (
    event_id TEXT PRIMARY KEY,
    expires_at INTEGER NOT NULL
);

ALTER TABLE operations ADD COLUMN correlation_id TEXT NOT NULL DEFAULT '';

CREATE TABLE draft_idempotency (
    principal_scope TEXT NOT NULL,
    key TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    draft_id TEXT NOT NULL REFERENCES agent_drafts(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(principal_scope, key)
);
