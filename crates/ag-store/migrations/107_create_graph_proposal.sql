-- Proposals cannot be dispatched until exact-version approval publishes a graph.
CREATE TABLE graph_proposal (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(trim(id)) > 0),
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK (version > 0),
    capture TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX graph_proposal_session_id_idx ON graph_proposal (session_id);

CREATE TABLE graph_proposal_version (
    graph_proposal_id TEXT NOT NULL REFERENCES graph_proposal(id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK (version > 0),
    snapshot TEXT NOT NULL,
    is_generating INTEGER NOT NULL CHECK (is_generating IN (0, 1)),
    is_cancelled INTEGER NOT NULL DEFAULT 0 CHECK (is_cancelled IN (0, 1)),
    approval TEXT,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (graph_proposal_id, version)
);
