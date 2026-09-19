CREATE TABLE session_command (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    turn_position INTEGER NOT NULL,
    owner_token BLOB NOT NULL,
    intent TEXT NOT NULL,
    outcome TEXT,
    reconciled INTEGER NOT NULL DEFAULT 0 CHECK (reconciled IN (0, 1)),
    unresolved INTEGER NOT NULL DEFAULT 1 CHECK (unresolved IN (0, 1)),
    FOREIGN KEY (session_id, turn_position)
        REFERENCES session_turn (session_id, turn_position)
);

CREATE INDEX session_command_admission
ON session_command (session_id, unresolved, reconciled);
