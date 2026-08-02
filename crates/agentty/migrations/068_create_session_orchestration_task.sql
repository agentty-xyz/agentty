-- One planned, running, or settled child task inside one orchestration.
--
-- Rows are written when the controller proposes a plan, before any child
-- session exists, so an approved plan survives restart and a retry that reuses
-- `task_key` replaces the previous attempt instead of fanning out a duplicate.
-- `child_session_id` stays NULL until the child is created and is cleared if
-- that session is deleted, which is what restart re-linking reconciles against.
CREATE TABLE session_orchestration_task (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_orchestration_id INTEGER NOT NULL REFERENCES session_orchestration(id) ON DELETE CASCADE,
    task_key TEXT NOT NULL,
    child_session_id TEXT REFERENCES session(id) ON DELETE SET NULL,
    title TEXT NOT NULL,
    prompt TEXT NOT NULL,
    touched_areas TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    result_summary TEXT,
    last_error TEXT,
    created_at INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX session_orchestration_task_orchestration_id_task_key_idx
ON session_orchestration_task (session_orchestration_id, task_key);

CREATE INDEX session_orchestration_task_child_session_id_idx
ON session_orchestration_task (child_session_id);
