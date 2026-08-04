CREATE TABLE community_configs (
    id TEXT PRIMARY KEY,
    document TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE agent_specs (
    id TEXT PRIMARY KEY,
    community_config_id TEXT NOT NULL REFERENCES community_configs(id) ON DELETE RESTRICT,
    document TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE operations (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    agent_id TEXT REFERENCES agent_specs(id) ON DELETE SET NULL,
    error_code TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE audit_records (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    occurred_at INTEGER NOT NULL,
    actor_principal TEXT NOT NULL,
    authentication_method TEXT NOT NULL,
    community_config_id TEXT,
    operation_id TEXT,
    correlation_id TEXT NOT NULL,
    idempotency_key TEXT,
    action TEXT NOT NULL,
    subject_type TEXT NOT NULL,
    subject_id TEXT,
    outcome TEXT NOT NULL,
    detail TEXT
);

CREATE TABLE idempotency_keys (
    scope TEXT NOT NULL,
    key TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (scope, key)
);

CREATE INDEX operations_status_idx ON operations(status, created_at);
CREATE INDEX audit_records_subject_idx ON audit_records(subject_type, subject_id, sequence);
