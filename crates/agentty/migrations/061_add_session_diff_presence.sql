ALTER TABLE session
ADD COLUMN has_diff INTEGER;

UPDATE session
SET has_diff = CASE
    WHEN added_lines > 0 OR deleted_lines > 0 THEN 1
    -- Legacy zero-line rows may contain binary- or metadata-only changes, so
    -- retain unknown state until a real diff refresh determines availability.
    ELSE NULL
END
WHERE has_diff IS NULL;
