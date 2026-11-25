-- An append-only revision of a graph: the revision before it plus `nodes`.
-- Older revisions that only expansions follow keep addressing the current
-- snapshot, because none of their nodes changed. The expansion identity is
-- the change's idempotency key: redelivery never appends twice.
CREATE TABLE execution_graph_expansion (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(trim(id)) > 0),
    execution_graph_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 1),
    nodes TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (execution_graph_id, revision),
    FOREIGN KEY (execution_graph_id, revision)
        REFERENCES execution_graph_revision(execution_graph_id, revision) ON DELETE CASCADE
);
