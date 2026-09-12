ALTER TABLE session_write
ADD COLUMN repository_root_bytes BLOB NOT NULL DEFAULT X'';

UPDATE session_write
SET repository_root_bytes = CAST(repository_root AS BLOB)
WHERE repository_root_bytes = X'';

ALTER TABLE session_write DROP COLUMN repository_root;
ALTER TABLE session_write RENAME COLUMN repository_root_bytes TO repository_root;

ALTER TABLE session_write ADD COLUMN acknowledged_by_turn INTEGER;
