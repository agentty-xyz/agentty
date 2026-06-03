ALTER TABLE session
ADD COLUMN parent_session_id TEXT REFERENCES session(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_session_parent_session_id
ON session(parent_session_id);
