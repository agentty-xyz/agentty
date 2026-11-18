DROP INDEX IF EXISTS idx_session_parent_session_id;

ALTER TABLE session DROP COLUMN parent_session_id;
ALTER TABLE session DROP COLUMN stack_base_commit_hash;
