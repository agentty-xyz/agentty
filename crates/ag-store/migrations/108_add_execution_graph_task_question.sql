-- A generic graph node's chat question commits with its task turn. The
-- message id orders pending questions so a chat reply answers the latest one.
ALTER TABLE execution_graph_task_turn
ADD COLUMN session_message_id INTEGER REFERENCES session_message(id) ON DELETE SET NULL;
