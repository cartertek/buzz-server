CREATE TABLE agent_retention (
    agent_id TEXT PRIMARY KEY REFERENCES agent_specs(id) ON DELETE CASCADE,
    deleted_at INTEGER NOT NULL,
    purge_after INTEGER NOT NULL
);

CREATE INDEX agent_retention_purge_after_idx ON agent_retention(purge_after, agent_id);

CREATE TABLE purged_agent_tombstones (
    agent_id TEXT PRIMARY KEY,
    purged_at INTEGER NOT NULL,
    purge_operation_id TEXT NOT NULL
);
