ALTER TABLE session ADD COLUMN provider_context TEXT
CHECK (provider_context IS NULL OR length(provider_context) BETWEEN 1 AND 256);
