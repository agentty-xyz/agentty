CREATE TABLE session_checkpoint (
    session_id TEXT PRIMARY KEY NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK (version > 0),
    covered_through INTEGER NOT NULL CHECK (covered_through >= 0),
    model_generation INTEGER NOT NULL CHECK (model_generation >= 0),
    provider TEXT,
    model TEXT,
    summary TEXT NOT NULL CHECK (json_valid(summary)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK (
        (provider IS NULL AND model IS NULL)
        OR (provider IS NOT NULL AND model IS NOT NULL)
    )
);
