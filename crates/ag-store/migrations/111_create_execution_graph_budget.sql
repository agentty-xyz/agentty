-- Provider-call limits are fixed at a graph's first launch. Spending is
-- recorded as it happens, so resuming after a restart never replenishes it.
CREATE TABLE execution_graph_budget (
    execution_graph_id TEXT NOT NULL REFERENCES execution_graph(id) ON DELETE CASCADE,
    budget_id TEXT NOT NULL CHECK (length(trim(budget_id)) > 0),
    provider_call_limit INTEGER NOT NULL CHECK (provider_call_limit >= 0),
    spent_provider_call_count INTEGER NOT NULL DEFAULT 0
        CHECK (spent_provider_call_count BETWEEN 0 AND provider_call_limit),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (execution_graph_id, budget_id)
);
