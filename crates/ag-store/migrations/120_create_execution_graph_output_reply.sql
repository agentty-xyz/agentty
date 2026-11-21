-- A chat reply to one exact output version awaiting acceptance, recorded with
-- its visible prompt. `execution` is the producer execution that revision
-- guidance targets, and the reply identity is that guidance's action identity,
-- so a recovered revision redelivers the same continuation. The first decision
-- is final; settlement marks that its action was attempted, so restart
-- recovery decides or applies only unsettled replies.
CREATE TABLE execution_graph_output_reply (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(trim(id)) > 0),
    execution_graph_result_id TEXT NOT NULL
        REFERENCES execution_graph_result(id) ON DELETE CASCADE,
    execution TEXT NOT NULL,
    message TEXT NOT NULL CHECK (length(trim(message)) > 0),
    attachments TEXT NOT NULL,
    outcome TEXT CHECK (outcome IN ('accept', 'revise')),
    is_settled INTEGER NOT NULL DEFAULT 0 CHECK (is_settled IN (0, 1)),
    created_at INTEGER NOT NULL
);
CREATE INDEX execution_graph_output_reply_execution_graph_result_id_idx
    ON execution_graph_output_reply (execution_graph_result_id);
