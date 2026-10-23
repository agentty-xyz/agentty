-- Graph snapshots and output attestations never own execution lifecycle.
CREATE TABLE execution_graph (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(trim(id)) > 0),
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK (revision > 0)
);
CREATE INDEX execution_graph_session_id_idx ON execution_graph (session_id);

CREATE TABLE execution_graph_revision (
    execution_graph_id TEXT NOT NULL REFERENCES execution_graph(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK (revision > 0),
    snapshot TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (execution_graph_id, revision)
);

CREATE TABLE execution_graph_execution (
    id INTEGER PRIMARY KEY NOT NULL,
    execution_graph_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    node_id TEXT NOT NULL,
    task_attempt_id TEXT UNIQUE REFERENCES task_attempt(id) ON DELETE CASCADE,
    agent_run_id TEXT UNIQUE REFERENCES agent_run(id) ON DELETE CASCADE,
    reference TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (execution_graph_id, revision)
        REFERENCES execution_graph_revision(execution_graph_id, revision) ON DELETE CASCADE,
    CHECK ((task_attempt_id IS NOT NULL) != (agent_run_id IS NOT NULL))
);
CREATE INDEX execution_graph_execution_graph_idx
    ON execution_graph_execution (execution_graph_id, revision, node_id);

CREATE TABLE execution_graph_result (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(trim(id)) > 0),
    execution_graph_execution_id INTEGER NOT NULL
        REFERENCES execution_graph_execution(id) ON DELETE CASCADE,
    execution_graph_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    output_name TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version > 0),
    reference TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (execution_graph_id, node_id, output_name, version),
    UNIQUE (execution_graph_execution_id, output_name)
);

CREATE TABLE execution_graph_input (
    execution_graph_execution_id INTEGER NOT NULL
        REFERENCES execution_graph_execution(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    position INTEGER NOT NULL,
    execution_graph_result_id TEXT NOT NULL
        REFERENCES execution_graph_result(id) ON DELETE CASCADE,
    PRIMARY KEY (execution_graph_execution_id, name),
    UNIQUE (execution_graph_execution_id, position)
);

CREATE TABLE execution_graph_evidence (
    execution_graph_result_id TEXT NOT NULL
        REFERENCES execution_graph_result(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('validation', 'acceptance')),
    actor TEXT NOT NULL CHECK (length(trim(actor)) > 0),
    reason TEXT NOT NULL CHECK (length(trim(reason)) > 0),
    recorded_at INTEGER NOT NULL,
    PRIMARY KEY (execution_graph_result_id, kind)
);
