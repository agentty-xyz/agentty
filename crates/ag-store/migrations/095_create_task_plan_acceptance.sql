CREATE TABLE task_plan_acceptance (
    task_attempt_id TEXT PRIMARY KEY NOT NULL REFERENCES task_attempt(id) ON DELETE CASCADE,
    plan TEXT NOT NULL CHECK (length(trim(plan)) > 0),
    accepted_at INTEGER NOT NULL
);
