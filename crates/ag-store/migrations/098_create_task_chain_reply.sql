CREATE TABLE task_chain_reply (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(trim(id)) > 0),
    task_chain_id TEXT NOT NULL REFERENCES task_chain(id) ON DELETE CASCADE,
    message TEXT NOT NULL CHECK (length(trim(message)) > 0),
    created_at INTEGER NOT NULL
);
