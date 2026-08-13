CREATE TABLE session_orchestration (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    controller_session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    status TEXT NOT NULL,
    max_parallelism INTEGER NOT NULL,
    created_at INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX session_orchestration_controller_session_id_idx
ON session_orchestration (controller_session_id);
