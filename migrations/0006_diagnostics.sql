CREATE TABLE reconciliation_journal (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    occurred_at INTEGER NOT NULL,
    actor TEXT NOT NULL,
    operation_id TEXT,
    correlation_id TEXT,
    agent_id TEXT,
    launch_id TEXT,
    stage TEXT NOT NULL,
    desired_state TEXT,
    observed_state TEXT,
    outcome TEXT NOT NULL,
    reason TEXT,
    pid INTEGER,
    process_start_ticks INTEGER,
    command_path TEXT,
    exit_code INTEGER,
    follow_up TEXT
);
CREATE INDEX reconciliation_journal_agent_idx ON reconciliation_journal(agent_id, sequence);
CREATE INDEX reconciliation_journal_operation_idx ON reconciliation_journal(operation_id, sequence);

CREATE TABLE health_history (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    occurred_at INTEGER NOT NULL,
    agent_id TEXT NOT NULL,
    launch_id TEXT NOT NULL,
    workspace TEXT NOT NULL,
    supervisor_pid INTEGER NOT NULL,
    process_start_ticks INTEGER,
    command_path TEXT,
    observed_state TEXT NOT NULL,
    exit_code INTEGER
);
CREATE INDEX health_history_agent_idx ON health_history(agent_id, sequence);
