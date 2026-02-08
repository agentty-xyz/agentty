ALTER TABLE session ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE session ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
UPDATE session
SET created_at = CAST(strftime('%s', 'now') AS INTEGER),
    updated_at = CAST(strftime('%s', 'now') AS INTEGER)
WHERE created_at = 0 OR updated_at = 0;

CREATE TRIGGER update_session_insert_timestamps
AFTER INSERT ON session
WHEN NEW.created_at = 0 OR NEW.updated_at = 0
BEGIN
    UPDATE session
    SET created_at = CASE
            WHEN NEW.created_at = 0 THEN CAST(strftime('%s', 'now') AS INTEGER)
            ELSE NEW.created_at
        END,
        updated_at = CASE
            WHEN NEW.updated_at = 0 THEN CAST(strftime('%s', 'now') AS INTEGER)
            ELSE NEW.updated_at
        END
    WHERE rowid = NEW.rowid;
END;

CREATE TRIGGER update_session_updated_at
AFTER UPDATE ON session
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE session
    SET updated_at = CAST(strftime('%s', 'now') AS INTEGER)
    WHERE rowid = NEW.rowid;
END;
