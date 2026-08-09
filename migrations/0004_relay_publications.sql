CREATE TABLE relay_projection_state (
    community_config_id TEXT NOT NULL,
    projection_kind TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    relay_url TEXT NOT NULL,
    owner_pubkey TEXT NOT NULL,
    d_tag TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (community_config_id, projection_kind, subject_id)
);
CREATE INDEX relay_projection_state_subject_idx ON relay_projection_state(projection_kind, subject_id);
CREATE TABLE relay_publication_outbox (
    id TEXT PRIMARY KEY,
    action TEXT NOT NULL,
    community_config_id TEXT,
    relay_url TEXT NOT NULL,
    owner_pubkey TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    d_tag TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    UNIQUE(action, relay_url, owner_pubkey, subject_id, d_tag)
);
CREATE INDEX relay_publication_outbox_owner_idx ON relay_publication_outbox(owner_pubkey, created_at);
