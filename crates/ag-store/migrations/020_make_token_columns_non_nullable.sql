UPDATE session
SET input_tokens = 0
WHERE input_tokens IS NULL;

UPDATE session
SET output_tokens = 0
WHERE output_tokens IS NULL;

ALTER TABLE session ADD COLUMN input_tokens_new INTEGER NOT NULL DEFAULT 0;
ALTER TABLE session ADD COLUMN output_tokens_new INTEGER NOT NULL DEFAULT 0;

UPDATE session
SET input_tokens_new = input_tokens,
    output_tokens_new = output_tokens;

ALTER TABLE session DROP COLUMN input_tokens;
ALTER TABLE session DROP COLUMN output_tokens;

ALTER TABLE session RENAME COLUMN input_tokens_new TO input_tokens;
ALTER TABLE session RENAME COLUMN output_tokens_new TO output_tokens;
