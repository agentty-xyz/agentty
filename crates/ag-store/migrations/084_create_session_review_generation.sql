-- Fence checkpoint writes so superseded or completed reviews cannot restore them.
CREATE TABLE session_review_generation (
    session_id TEXT PRIMARY KEY NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    generation TEXT NOT NULL
);
