CREATE TABLE task_agent_context (
    id TEXT PRIMARY KEY NOT NULL DEFAULT (lower(hex(randomblob(16)))),
    task_id TEXT NOT NULL UNIQUE REFERENCES task(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('planner', 'implementer')),
    agent TEXT NOT NULL,
    model TEXT NOT NULL,
    input TEXT NOT NULL
);
