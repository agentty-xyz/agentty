-- The milestone of the task a session holds or that a conflict names, as the
-- platform's Sync answered it: all three columns are set, or none is.
ALTER TABLE session_platform_task ADD COLUMN milestone_number INTEGER;
ALTER TABLE session_platform_task ADD COLUMN milestone_title TEXT;
ALTER TABLE session_platform_task ADD COLUMN milestone_goal TEXT;
