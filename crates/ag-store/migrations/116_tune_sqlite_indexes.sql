-- Unfinished-operation recovery scans only queued and running rows in queue
-- order, so the partial index stays small while finished history grows.
CREATE INDEX session_operation_unfinished_idx
ON session_operation (queued_at, id)
WHERE status IN ('queued', 'running');

-- Session deletion cascades or nulls these references; without an index each
-- deleted parent row scans the child table.
CREATE INDEX agent_run_session_id_idx ON agent_run (session_id);

CREATE INDEX execution_graph_task_turn_session_message_id_idx
ON execution_graph_task_turn (session_message_id);

CREATE INDEX execution_graph_input_execution_graph_result_id_idx
ON execution_graph_input (execution_graph_result_id);

CREATE INDEX execution_graph_continuation_execution_graph_id_idx
ON execution_graph_continuation (execution_graph_id);

-- These indexes duplicate another index or a leading prefix of a unique
-- constraint, so they only add write cost.
DROP INDEX idx_session_activity_created_at;
DROP INDEX session_activity_session_id_idx;
DROP INDEX session_message_session_id_idx;
DROP INDEX session_usage_session_id_idx;
