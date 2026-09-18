ALTER TABLE session_turn ADD COLUMN host_id TEXT;
ALTER TABLE session_turn ADD COLUMN host_request TEXT
CHECK (host_request IS NULL OR json_valid(host_request));
ALTER TABLE session_turn ADD COLUMN terminal_outcome TEXT
CHECK (terminal_outcome IS NULL OR json_valid(terminal_outcome));

CREATE UNIQUE INDEX session_turn_host_id_idx
ON session_turn (session_id, host_id)
WHERE host_id IS NOT NULL;
