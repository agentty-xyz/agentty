ALTER TABLE project ADD COLUMN created_at      INTEGER NOT NULL DEFAULT 0;
ALTER TABLE project ADD COLUMN display_name    TEXT;
ALTER TABLE project ADD COLUMN is_favorite     INTEGER NOT NULL DEFAULT 0;
ALTER TABLE project ADD COLUMN last_opened_at  INTEGER;
ALTER TABLE project ADD COLUMN updated_at      INTEGER NOT NULL DEFAULT 0;

UPDATE project SET created_at = unixepoch(), updated_at = unixepoch() WHERE created_at = 0;
