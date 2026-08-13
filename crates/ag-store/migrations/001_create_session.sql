CREATE TABLE IF NOT EXISTS session (
    name        TEXT PRIMARY KEY NOT NULL,
    agent       TEXT NOT NULL,
    base_branch TEXT NOT NULL
);
