ALTER TABLE session_turn ADD COLUMN turn_options TEXT
CHECK (turn_options IS NULL OR json_valid(turn_options));
