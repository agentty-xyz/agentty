CREATE TABLE harness_turn_report (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT REFERENCES session(id) ON DELETE SET NULL,
    model TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    report TEXT NOT NULL
);

CREATE INDEX harness_turn_report_session_id_idx ON harness_turn_report (session_id);
