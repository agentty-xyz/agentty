CREATE TABLE session_message (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    kind TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX session_message_session_id_position_idx
ON session_message (session_id, position);

CREATE INDEX session_message_session_id_idx
ON session_message (session_id);
