-- Project deletion nulls `agent_run.project_id`; without an index each deleted
-- project scans every agent run.
CREATE INDEX agent_run_project_id_idx ON agent_run (project_id);
