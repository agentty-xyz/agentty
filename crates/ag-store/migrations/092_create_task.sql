CREATE TABLE task (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(trim(id)) > 0),
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    objective TEXT NOT NULL CHECK (length(trim(objective)) > 0),
    created_at INTEGER NOT NULL,
    completed_at INTEGER
);

CREATE INDEX task_session_id_idx ON task (session_id);

CREATE TABLE task_attempt (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(trim(id)) > 0),
    task_id TEXT NOT NULL REFERENCES task(id) ON DELETE CASCADE,
    session_operation_id TEXT NOT NULL UNIQUE REFERENCES session_operation(id) ON DELETE CASCADE,
    message TEXT NOT NULL CHECK (length(trim(message)) > 0),
    is_visible INTEGER NOT NULL CHECK (is_visible IN (0, 1))
);

CREATE INDEX task_attempt_task_id_idx ON task_attempt (task_id);

-- Attempt history is immutable even if an ordinary durable-message caller
-- tries to reopen its operation through the legacy retry path.
CREATE TRIGGER task_attempt_no_reopen
BEFORE UPDATE ON session_operation
WHEN OLD.status IN ('done', 'failed', 'canceled')
 AND NEW.status IN ('queued', 'running')
 AND EXISTS (SELECT 1 FROM task_attempt WHERE session_operation_id = OLD.id)
BEGIN
    SELECT RAISE(ABORT, 'Task attempts require a new attempt id to retry');
END;
