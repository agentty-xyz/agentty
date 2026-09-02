CREATE TABLE session (
    id TEXT PRIMARY KEY NOT NULL,
    provider TEXT,
    model TEXT,
    output_schema TEXT NOT NULL,
    system_prompt TEXT,
    max_history_bytes INTEGER NOT NULL CHECK (max_history_bytes > 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK (
        (provider IS NULL AND model IS NULL)
        OR (provider IS NOT NULL AND model IS NOT NULL)
    )
);

CREATE TABLE session_message (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    turn_position INTEGER NOT NULL,
    message_position INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    retained_bytes INTEGER NOT NULL CHECK (retained_bytes >= 0),
    created_at INTEGER NOT NULL,
    UNIQUE (session_id, turn_position, message_position)
);

CREATE INDEX session_message_session_id_turn_position_idx
ON session_message (session_id, turn_position);
