ALTER TABLE session RENAME COLUMN personality_id TO agent_profile_id;
ALTER TABLE session RENAME COLUMN applied_personality_id TO applied_agent_profile_id;
ALTER TABLE session RENAME COLUMN applied_personality_prompt_hash TO applied_agent_profile_prompt_hash;

ALTER TABLE task_agent_context ADD COLUMN definition TEXT;
ALTER TABLE task_chain ADD COLUMN agent_definitions TEXT;
