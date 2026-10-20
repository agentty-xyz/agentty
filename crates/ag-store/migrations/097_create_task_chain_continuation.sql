CREATE TABLE task_chain_continuation (
    task_attempt_id TEXT PRIMARY KEY NOT NULL REFERENCES task_attempt(id) ON DELETE CASCADE,
    expected_task_attempt_id TEXT NOT NULL REFERENCES task_attempt(id) ON DELETE CASCADE
);
