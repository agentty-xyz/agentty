ALTER TABLE session
ADD COLUMN personality_id TEXT;

ALTER TABLE session
ADD COLUMN applied_personality_id TEXT;

ALTER TABLE session
ADD COLUMN applied_personality_prompt_hash TEXT;
