-- A user-initiated increase of one graph budget's provider calls. The budget's
-- `provider_call_limit` includes every extension, so the launch limit is that
-- value minus their sum. The extension identity is the action's idempotency
-- key: redelivery never adds calls twice.
CREATE TABLE execution_graph_budget_extension (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(trim(id)) > 0),
    execution_graph_id TEXT NOT NULL,
    budget_id TEXT NOT NULL,
    additional_provider_call_count INTEGER NOT NULL
        CHECK (additional_provider_call_count > 0),
    created_at INTEGER NOT NULL,
    FOREIGN KEY (execution_graph_id, budget_id)
        REFERENCES execution_graph_budget(execution_graph_id, budget_id) ON DELETE CASCADE
);
CREATE INDEX execution_graph_budget_extension_execution_graph_id_budget_id_idx
    ON execution_graph_budget_extension (execution_graph_id, budget_id);
