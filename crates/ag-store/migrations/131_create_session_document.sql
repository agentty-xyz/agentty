-- One shared notes document per session, which the graph nodes of the session
-- read and add to. The row holds the current body and its version; each
-- accepted write is an entry. Graph structure, executions, and evidence stay
-- in their own tables. Rows disappear with their session.
CREATE TABLE session_document (
    session_id TEXT PRIMARY KEY NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK (version > 0),
    body TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

-- One accepted write: a note appended to the body, or a replacement of the
-- whole body. A graph node, the one author that runs as an execution, records
-- its non-empty graph, node, and execution, and each of its notes records its
-- position in the response, so a redelivered response appends nothing twice.
-- Other authors, the user and the session's main chat, record none of them.
CREATE TABLE session_document_entry (
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK (version > 0),
    kind TEXT NOT NULL CHECK (kind IN ('note', 'replace')),
    author_kind TEXT NOT NULL CHECK (author_kind IN ('graph_node', 'main_chat', 'user')),
    graph_id TEXT,
    node_id TEXT,
    execution_id TEXT,
    note_index INTEGER CHECK (note_index >= 0),
    text TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, version),
    CHECK ((author_kind = 'graph_node') = (execution_id IS NOT NULL)),
    CHECK (graph_id <> '' AND node_id <> '' AND execution_id <> ''),
    CHECK ((graph_id IS NULL) = (execution_id IS NULL)),
    CHECK ((node_id IS NULL) = (execution_id IS NULL)),
    CHECK ((note_index IS NOT NULL) = (execution_id IS NOT NULL AND kind = 'note'))
);

CREATE UNIQUE INDEX session_document_entry_execution_note_idx
    ON session_document_entry (session_id, execution_id, note_index)
    WHERE note_index IS NOT NULL;
