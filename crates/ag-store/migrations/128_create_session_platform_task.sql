CREATE TABLE session_platform_task (
    session_id TEXT PRIMARY KEY NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('checking', 'interrupted', 'conflict', 'resolved')),
    platform_url TEXT NOT NULL,
    title TEXT NOT NULL,
    brief TEXT NOT NULL,
    not_duplicates TEXT NOT NULL DEFAULT '',
    pending_prompt TEXT,
    task_project TEXT,
    task_number INTEGER,
    task_title TEXT,
    task_url TEXT,
    claim_id TEXT,
    holder_session TEXT,
    holder_last_active_at INTEGER
);

CREATE INDEX session_platform_task_state_idx
    ON session_platform_task (state);

CREATE INDEX session_platform_task_claim_id_idx
    ON session_platform_task (claim_id)
    WHERE claim_id IS NOT NULL;
