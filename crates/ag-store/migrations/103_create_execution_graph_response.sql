-- Response evidence is independent of authoritative operation/run status.
-- Retain it even when a node declares no named outputs.
ALTER TABLE execution_graph_execution
ADD COLUMN is_dispatch_claimed INTEGER NOT NULL DEFAULT 0
    CHECK (is_dispatch_claimed IN (0, 1));

CREATE TABLE execution_graph_response (
    execution_graph_execution_id INTEGER PRIMARY KEY NOT NULL
        REFERENCES execution_graph_execution(id) ON DELETE CASCADE,
    body TEXT NOT NULL
);

-- Static graph tasks cannot acquire attempts through legacy task submission.
-- Explicit graph retry/continuation needs a separate admission contract.
CREATE TRIGGER execution_graph_task_attempt_guard
BEFORE INSERT ON task_attempt
WHEN EXISTS (
    SELECT 1 FROM execution_graph_execution e
    JOIN task_attempt a ON a.id = e.task_attempt_id
    WHERE a.task_id = NEW.task_id AND e.is_dispatch_claimed = 1
)
BEGIN
    SELECT RAISE(ABORT, 'Graph task attempts require graph admission');
END;
