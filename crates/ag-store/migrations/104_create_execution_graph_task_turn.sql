-- Observed task responses survive failed post-processing without releasing gates.
CREATE TABLE execution_graph_task_turn (
    execution_graph_execution_id INTEGER PRIMARY KEY NOT NULL
        REFERENCES execution_graph_execution(id) ON DELETE CASCADE,
    metadata TEXT NOT NULL
);
