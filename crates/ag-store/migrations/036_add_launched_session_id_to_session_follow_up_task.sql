ALTER TABLE session_follow_up_task
ADD COLUMN launched_session_id TEXT REFERENCES session(id) ON DELETE SET NULL;
