DROP TRIGGER IF EXISTS update_session_insert_timestamps;
DROP TRIGGER IF EXISTS update_session_updated_at;

ALTER TABLE session_usage RENAME TO session_usage_legacy;
DROP INDEX IF EXISTS session_usage_session_id_idx;

CREATE TABLE session_usage (
    session_id TEXT REFERENCES session(id) ON DELETE SET NULL,
    model TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    invocation_count INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    UNIQUE(session_id, model)
);

INSERT INTO session_usage (
    session_id,
    model,
    created_at,
    input_tokens,
    invocation_count,
    output_tokens
)
SELECT session_id,
       model,
       created_at,
       input_tokens,
       invocation_count,
       output_tokens
FROM session_usage_legacy;

DROP TABLE session_usage_legacy;

CREATE INDEX session_usage_session_id_idx ON session_usage (session_id);
