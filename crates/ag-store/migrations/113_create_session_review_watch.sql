CREATE TABLE session_review_watch (
    session_id TEXT PRIMARY KEY NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    handled_keys TEXT NOT NULL DEFAULT ''
);
