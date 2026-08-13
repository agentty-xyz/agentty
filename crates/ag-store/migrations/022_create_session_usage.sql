CREATE TABLE session_usage (
    session_id TEXT REFERENCES session(id) ON DELETE SET NULL,
    model TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    input_tokens INTEGER NOT NULL DEFAULT 0,
    invocation_count INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    UNIQUE(session_id, model)
);

CREATE INDEX session_usage_session_id_idx ON session_usage (session_id);
