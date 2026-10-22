-- Migration 100 removed the information identifying conversations with applied
-- personalities. Mark every existing session once, including native harness
-- sessions whose durable history can outlive their provider conversation ID.
-- Successful ordinary-turn persistence replaces this marker with the normal
-- instruction bootstrap key; failed turns retain it for retry.
UPDATE session
SET app_server_instruction_provider_conversation_id = 'agentty:clear-legacy-session-agent:v1';
