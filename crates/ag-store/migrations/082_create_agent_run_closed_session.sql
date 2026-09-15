-- Admission tombstones outlive host sessions so evicted worker state cannot
-- allow detached tasks to restart model work after cancellation or deletion.
CREATE TABLE agent_run_closed_session (
    session_id TEXT PRIMARY KEY NOT NULL
);
