CREATE TABLE agent_run (
    id TEXT PRIMARY KEY NOT NULL,
    parent_id TEXT,
    session_id TEXT REFERENCES session(id) ON DELETE SET NULL,
    project_id INTEGER REFERENCES project(id) ON DELETE SET NULL,
    folder TEXT NOT NULL,
    purpose TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'completed', 'failed', 'canceled')),
    queued_at INTEGER NOT NULL,
    started_at INTEGER,
    heartbeat_at INTEGER,
    finished_at INTEGER,
    last_error TEXT
);

CREATE INDEX idx_agent_run_unfinished ON agent_run(status)
WHERE status IN ('queued', 'running');
