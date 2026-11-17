-- Each turn records the usage and cost of its completed model requests as
-- JSON, whether the turn completes, fails, or is interrupted. Turns recorded
-- before this migration keep NULL and read their usage from a stored outcome,
-- or as unknown without one.
ALTER TABLE session_turn ADD COLUMN usage TEXT CHECK (usage IS NULL OR json_valid(usage));
