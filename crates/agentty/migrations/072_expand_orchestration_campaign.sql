-- Persist the final campaign schema introduced by the orchestration-board
-- workflow: planning evidence, follow-up routing, question ownership,
-- verification evidence, integration choice, and archived child diffs.
ALTER TABLE session_orchestration ADD COLUMN goal_statement TEXT NOT NULL DEFAULT '';
ALTER TABLE session_orchestration ADD COLUMN verification_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE session_orchestration ADD COLUMN relayed_question_task_id INTEGER
    REFERENCES session_orchestration_task(id) ON DELETE SET NULL;
ALTER TABLE session_orchestration ADD COLUMN integration_approach TEXT NOT NULL DEFAULT 'LocalMerge';

ALTER TABLE session_orchestration_task ADD COLUMN acceptance_criteria TEXT NOT NULL DEFAULT '[]';
ALTER TABLE session_orchestration_task ADD COLUMN merge_position INTEGER NOT NULL DEFAULT 0;
ALTER TABLE session_orchestration_task ADD COLUMN verification_verdict TEXT;
ALTER TABLE session_orchestration_task ADD COLUMN verification_reason TEXT;
ALTER TABLE session_orchestration_task ADD COLUMN continuation_prompt TEXT;
ALTER TABLE session_orchestration_task ADD COLUMN continuation_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE session_orchestration_task ADD COLUMN infrastructure_retry_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE session_orchestration_task ADD COLUMN area_violations TEXT NOT NULL DEFAULT '[]';
ALTER TABLE session_orchestration_task ADD COLUMN areas_compliant INTEGER;

ALTER TABLE session ADD COLUMN archived_diff TEXT;

-- Existing linked children were created before managed-worker roles existed.
UPDATE session
SET role = 'OrchestrationWorker'
WHERE orchestration_task_id IS NOT NULL;
