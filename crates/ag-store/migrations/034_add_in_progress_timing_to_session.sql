ALTER TABLE session
ADD COLUMN in_progress_total_seconds INTEGER NOT NULL DEFAULT 0;

ALTER TABLE session
ADD COLUMN in_progress_started_at INTEGER;
