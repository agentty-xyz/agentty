-- Plan-then-implement workflows now run as execution graphs. The experimental
-- legacy workflow ledgers are discarded; sessions and individual task histories
-- remain intact, and no legacy workflow is replayed or converted into a graph.
-- Retired graph markers must not keep /plan disabled in retained sessions.
DELETE FROM session_message
WHERE kind = 'agent_graph'
  AND (session_id, content) IN (
      SELECT session_id, id FROM task_chain
  );

DROP TABLE task_chain_reply;
DROP TABLE task_chain_continuation;
DROP TABLE task_chain;

-- One natural-language reply bound to the exact execution it answered. The
-- first recorded decision is final, so redelivery reapplies the same
-- idempotent graph action instead of deciding again.
CREATE TABLE execution_graph_reply (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(trim(id)) > 0),
    execution_graph_id TEXT NOT NULL REFERENCES execution_graph(id) ON DELETE CASCADE,
    source_reference TEXT NOT NULL,
    prompt TEXT NOT NULL,
    outcome TEXT CHECK (outcome IN ('plan_accepted', 'plan_revised', 'continued')),
    action_id TEXT,
    created_at INTEGER NOT NULL,
    CHECK ((outcome IS NULL AND action_id IS NULL) OR outcome IS NOT NULL)
);
CREATE INDEX execution_graph_reply_graph_idx ON execution_graph_reply (execution_graph_id);
