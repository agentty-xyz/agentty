-- Session list queries report an archive with `archived_diff IS NOT NULL`,
-- which SQLite answers from the record header without reading the stored
-- text. Store blank archives as NULL so that check stays exact.
UPDATE session
SET archived_diff = NULL
WHERE archived_diff IS NOT NULL
  AND trim(archived_diff, char(9) || char(10) || char(13) || ' ') = '';
