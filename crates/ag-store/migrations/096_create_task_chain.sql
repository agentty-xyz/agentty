CREATE TABLE task_chain (
    id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    agent TEXT NOT NULL,
    model TEXT NOT NULL,
    user_request TEXT NOT NULL CHECK (length(trim(user_request)) > 0),
    planner_task_id TEXT NOT NULL UNIQUE REFERENCES task(id) ON DELETE CASCADE,
    implementer_task_id TEXT NOT NULL UNIQUE REFERENCES task(id) ON DELETE CASCADE,
    planner_task_attempt_id TEXT NOT NULL UNIQUE REFERENCES task_attempt(id) ON DELETE CASCADE,
    accepted_task_attempt_id TEXT REFERENCES task_attempt(id) ON DELETE CASCADE,
    canceled_at INTEGER,
    created_at INTEGER NOT NULL
);
