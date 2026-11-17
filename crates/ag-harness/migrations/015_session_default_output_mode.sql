-- A session's default output schema becomes optional: NULL selects plain-text
-- answers. SQLite cannot drop NOT NULL in place, so the column is copied into
-- a nullable one under the same name; every existing session keeps its schema.
ALTER TABLE session ADD COLUMN default_output_schema TEXT;
UPDATE session SET default_output_schema = output_schema
WHERE output_schema IS NOT NULL;
ALTER TABLE session DROP COLUMN output_schema;
ALTER TABLE session RENAME COLUMN default_output_schema TO output_schema;
