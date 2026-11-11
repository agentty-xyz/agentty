-- Direct Gemini CLI support was removed; Antigravity serves the same models.
-- Gemini ACP conversation ids cannot resume in Antigravity, so moved sessions
-- and task contexts start a fresh provider conversation. The legacy agent
-- clear marker survives because it governs the next ordinary turn's prompt.
UPDATE session
SET agent = 'antigravity',
    provider_conversation_id = NULL,
    app_server_instruction_provider_conversation_id = CASE
        WHEN app_server_instruction_provider_conversation_id
            = 'agentty:clear-legacy-session-agent:v1'
        THEN app_server_instruction_provider_conversation_id
        ELSE NULL
    END
WHERE agent = 'gemini';

UPDATE task_agent_context
SET agent = 'antigravity',
    provider_conversation_id = NULL,
    instruction_conversation_id = NULL
WHERE agent = 'gemini';

UPDATE project_setting
SET value = 'antigravity'
WHERE value = 'gemini'
  AND name IN ('DefaultSmartAgent', 'DefaultFastAgent', 'DefaultReviewAgent');

UPDATE setting
SET value = 'antigravity'
WHERE value = 'gemini'
  AND name IN ('DefaultSmartAgent', 'DefaultFastAgent', 'DefaultReviewAgent');
