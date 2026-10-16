-- Preserve campaign plans and reports as inert controller history before
-- retiring the coordinator's storage. Existing session transcripts, branches,
-- worktrees, review evidence, and archived diffs remain owned by their sessions.
INSERT INTO session_message (session_id, position, kind, content, created_at)
SELECT controller_session_id,
       COALESCE((SELECT MAX(position) FROM session_message
                 WHERE session_id = controller_session_id), -1) + 1,
       'workflow_notice',
       'Legacy orchestration retired. Linked sessions are now independent. Campaign history: '
       || json_group_array(json_object(
           'id', campaign.id,
           'goal', campaign.goal_statement,
           'status', campaign.status,
           'tasks', json((SELECT json_group_array(json_object(
               'key', task.task_key,
               'title', task.title,
               'prompt', task.prompt,
               'continuation_prompt', task.continuation_prompt,
               'acceptance_criteria', task.acceptance_criteria,
               'child_session_id', task.child_session_id,
               'status', task.status,
               'result_summary', task.result_summary,
               'research_report', task.research_report,
               'verification_reason', task.verification_reason,
               'last_error', task.last_error
           )) FROM session_orchestration_task AS task
           WHERE task.session_orchestration_id = campaign.id))
       )),
       MAX(campaign.updated_at)
FROM session_orchestration AS campaign
GROUP BY controller_session_id;

-- Saved first prompts must not restart retired work after workspace preparation.
INSERT INTO session_message (session_id, position, kind, content)
SELECT preparation.session_id,
       COALESCE((SELECT MAX(position) FROM session_message
                 WHERE session_id = preparation.session_id), -1) + 1,
       'workflow_notice',
       'Legacy orchestration retired. Unstarted prompt preserved: ' || preparation.prompt
FROM session_preparation AS preparation
JOIN session ON session.id = preparation.session_id
WHERE session.role IN ('Orchestrator', 'OrchestrationWorker', 'OrchestrationResearcher')
  AND preparation.prompt IS NOT NULL;
UPDATE session_preparation
SET prompt = NULL,
    state = CASE WHEN state = 'preparing' THEN 'failed' ELSE state END,
    error = CASE WHEN state = 'preparing' THEN 'Legacy orchestration retired' ELSE error END
WHERE session_id IN (SELECT id FROM session WHERE role IN (
    'Orchestrator', 'OrchestrationWorker', 'OrchestrationResearcher'
));

-- Retire queued coordinator work instead of replaying it as ordinary chat.
UPDATE session_operation
SET status = 'failed',
    last_error = 'Legacy orchestration retired',
    cancel_requested = 1,
    finished_at = COALESCE(finished_at, unixepoch())
WHERE status IN ('queued', 'running')
  AND session_id IN (SELECT id FROM session WHERE role IN (
      'Orchestrator', 'OrchestrationWorker', 'OrchestrationResearcher'
  ));

-- A researcher must not gain write access when its role discriminator goes away.
UPDATE session SET permission_mode = 'read_only'
WHERE role IN ('Orchestrator', 'OrchestrationResearcher');

-- Controllers may hold relayed child questions, not questions of their own.
UPDATE session SET questions = NULL WHERE role = 'Orchestrator';
-- Close active-work intervals before retiring their execution status, using
-- the same nonnegative elapsed-time accounting as normal status transitions.
UPDATE session
SET in_progress_total_seconds = in_progress_total_seconds
        + MAX(0, unixepoch() - in_progress_started_at),
    in_progress_started_at = NULL
WHERE role IN ('Orchestrator', 'OrchestrationWorker', 'OrchestrationResearcher')
  AND in_progress_started_at IS NOT NULL;
UPDATE session
SET status = CASE WHEN is_draft THEN 'Draft' ELSE 'Review' END
WHERE role IN ('Orchestrator', 'OrchestrationWorker', 'OrchestrationResearcher')
  AND status IN ('InProgress', 'Queued', 'Rebasing', 'Merging', 'AgentReview');
UPDATE session SET status = 'Review'
WHERE role = 'Orchestrator' AND status IN ('Draft', 'Question') AND NOT is_draft;
UPDATE session SET focused_review_status = 'Failed'
WHERE role IN ('Orchestrator', 'OrchestrationWorker', 'OrchestrationResearcher')
  AND focused_review_status = 'Pending';

DROP INDEX session_orchestration_task_id_idx;
ALTER TABLE session DROP COLUMN orchestration_task_id;
ALTER TABLE session DROP COLUMN role;
UPDATE session_orchestration SET relayed_question_task_id = NULL
WHERE relayed_question_task_id IS NOT NULL;
DROP TABLE session_orchestration_task;
DROP TABLE session_orchestration;
DELETE FROM setting
WHERE name IN ('OrchestrationParallelism', 'AutoApproveOrchestrationResearch');
DELETE FROM project_setting
WHERE name IN ('OrchestrationParallelism', 'AutoApproveOrchestrationResearch');
