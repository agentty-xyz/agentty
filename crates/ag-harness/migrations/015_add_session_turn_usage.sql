-- Each turn records the usage and cost of its completed model requests as
-- JSON, whether the turn completes, fails, or is interrupted. Turns recorded
-- before this migration keep NULL, which reads as unknown usage.
ALTER TABLE session_turn ADD COLUMN usage TEXT;
