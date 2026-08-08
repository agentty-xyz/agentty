-- Distinguish implementation work from temporary read-only research and keep
-- the bounded report after its child worktree is discarded.
ALTER TABLE session_orchestration_task
ADD COLUMN kind TEXT NOT NULL DEFAULT 'Implementation';

ALTER TABLE session_orchestration_task
ADD COLUMN research_report TEXT;
