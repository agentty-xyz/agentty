-- Durable reverse link used to recover a child created immediately before the
-- coordinator linked it back to its orchestration task.
ALTER TABLE session ADD COLUMN orchestration_task_id INTEGER
    REFERENCES session_orchestration_task(id) ON DELETE SET NULL;

CREATE UNIQUE INDEX session_orchestration_task_id_idx
ON session (orchestration_task_id)
WHERE orchestration_task_id IS NOT NULL;
