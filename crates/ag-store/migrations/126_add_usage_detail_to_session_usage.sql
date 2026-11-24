-- Records usage per model and purpose with the detail agents report. A NULL
-- detail is unknown: some invocation of the row did not report it. Rows
-- recorded before this migration count no detailed invocation, so
-- `detailed_invocation_count < invocation_count` marks rows whose details
-- stay unknown. SQLite cannot change a unique constraint in place, so the
-- table is rebuilt and existing rows become session turns.
ALTER TABLE session_usage RENAME TO session_usage_legacy;

CREATE TABLE session_usage (
    session_id TEXT REFERENCES session(id) ON DELETE SET NULL,
    model TEXT NOT NULL,
    purpose TEXT NOT NULL DEFAULT 'turn',
    created_at INTEGER NOT NULL,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    invocation_count INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    reasoning_tokens INTEGER,
    request_count INTEGER NOT NULL DEFAULT 0,
    unreported_request_count INTEGER NOT NULL DEFAULT 0,
    detailed_invocation_count INTEGER NOT NULL DEFAULT 0,
    -- Sum over the priced invocations; NULL until one is priced.
    cost_micro_usd INTEGER,
    -- list, host, agent_reported, or mixed when invocations differ.
    cost_basis TEXT,
    unpriced_invocation_count INTEGER NOT NULL DEFAULT 0,
    context_peak_tokens INTEGER,
    context_window_tokens INTEGER,
    UNIQUE(session_id, model, purpose)
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

-- Input tokens of the session's latest model request, and that model's
-- context window, kept across restarts.
ALTER TABLE session ADD COLUMN context_tokens INTEGER;
ALTER TABLE session ADD COLUMN context_window_tokens INTEGER;

-- Opaque cumulative usage totals an agent reported for the most recent
-- conversation in the session's worktree, from which the next continued turn
-- derives its own share, and the folder that conversation ran in.
ALTER TABLE session ADD COLUMN usage_checkpoint TEXT;
ALTER TABLE session ADD COLUMN usage_checkpoint_folder TEXT;
