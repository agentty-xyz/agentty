DROP INDEX idx_session_project_updated_at;

CREATE INDEX idx_session_project_updated_at
ON session (project_id, updated_at DESC, created_at DESC, id);
