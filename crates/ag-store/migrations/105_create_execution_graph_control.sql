-- Control intent is separate from operation/run execution status.
CREATE TABLE execution_graph_cancellation (
    execution_graph_id TEXT PRIMARY KEY NOT NULL
        REFERENCES execution_graph(id) ON DELETE CASCADE,
    recorded_at INTEGER NOT NULL
);

-- An explicit continuation is consumed by atomic dispatch exactly once.
CREATE TABLE execution_graph_continuation (
    id TEXT PRIMARY KEY NOT NULL,
    execution_graph_id TEXT NOT NULL REFERENCES execution_graph(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL,
    node_id TEXT NOT NULL,
    source_execution_graph_execution_id INTEGER NOT NULL UNIQUE
        REFERENCES execution_graph_execution(id) ON DELETE CASCADE,
    request TEXT NOT NULL,
    prompt TEXT NOT NULL,
    is_admitting INTEGER NOT NULL DEFAULT 0 CHECK (is_admitting IN (0, 1)),
    execution_graph_execution_id INTEGER UNIQUE REFERENCES execution_graph_execution(id) ON DELETE CASCADE
);

-- The flag exists only inside the graph claim's writer transaction. Legacy
-- task submission cannot consume a pending graph continuation by guessing its ID.
DROP TRIGGER execution_graph_task_attempt_guard;
CREATE TRIGGER execution_graph_task_attempt_guard
BEFORE INSERT ON task_attempt
WHEN EXISTS (
    SELECT 1 FROM execution_graph_execution e
    JOIN task_attempt a ON a.id = e.task_attempt_id
    WHERE a.task_id = NEW.task_id AND e.is_dispatch_claimed = 1
)
AND NOT EXISTS (
    SELECT 1 FROM execution_graph_continuation c
    JOIN execution_graph_execution e ON e.id = c.source_execution_graph_execution_id
    JOIN task_attempt a ON a.id = e.task_attempt_id
    WHERE c.id = NEW.id AND a.task_id = NEW.task_id
      AND c.is_admitting = 1 AND c.execution_graph_execution_id IS NULL
)
BEGIN
    SELECT RAISE(ABORT, 'Graph task attempts require graph admission');
END;
