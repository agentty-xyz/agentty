ALTER TABLE session ADD COLUMN model_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE session_turn ADD COLUMN model_snapshot TEXT;
