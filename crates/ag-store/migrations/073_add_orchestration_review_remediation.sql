-- Track focused-review completion independently from cached review text so
-- orchestrated workers can wait for success or failure without racing fan-in.
ALTER TABLE session ADD COLUMN focused_review_status TEXT;

UPDATE session
SET focused_review_status = 'Ready'
WHERE focused_review_text IS NOT NULL
  AND focused_review_text <> '';

-- Bound automatic focused-review remediation independently for every managed
-- task and preserve the counter across coordinator restarts.
ALTER TABLE session_orchestration_task
ADD COLUMN review_iteration INTEGER NOT NULL DEFAULT 0;
