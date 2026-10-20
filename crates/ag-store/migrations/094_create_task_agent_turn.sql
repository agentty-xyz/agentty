ALTER TABLE task_agent_context ADD COLUMN provider_conversation_id TEXT;
ALTER TABLE task_agent_context ADD COLUMN instruction_conversation_id TEXT;

CREATE TABLE task_agent_turn (
    task_attempt_id TEXT PRIMARY KEY NOT NULL REFERENCES task_attempt(id) ON DELETE CASCADE,
    assistant_message TEXT NOT NULL CHECK (length(trim(assistant_message)) > 0),
    input_tokens INTEGER NOT NULL CHECK (input_tokens >= 0),
    output_tokens INTEGER NOT NULL CHECK (output_tokens >= 0),
    provider_conversation_id TEXT,
    instruction_conversation_id TEXT,
    harness_turn_report TEXT
);
