-- A chat reply to one exact proposal version, recorded with its visible
-- prompt. The first decision is final; settlement marks that its action was
-- attempted, so restart recovery decides or applies only unsettled replies.
CREATE TABLE graph_proposal_reply (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(trim(id)) > 0),
    graph_proposal_id TEXT NOT NULL REFERENCES graph_proposal(id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK (version > 0),
    message TEXT NOT NULL CHECK (length(trim(message)) > 0),
    outcome TEXT CHECK (outcome IN ('approve', 'cancel', 'revise')),
    is_settled INTEGER NOT NULL DEFAULT 0 CHECK (is_settled IN (0, 1)),
    created_at INTEGER NOT NULL
);
CREATE INDEX graph_proposal_reply_graph_proposal_id_idx
    ON graph_proposal_reply (graph_proposal_id);
