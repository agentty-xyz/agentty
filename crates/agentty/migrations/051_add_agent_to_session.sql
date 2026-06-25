ALTER TABLE session
ADD COLUMN agent TEXT NOT NULL DEFAULT '';

UPDATE session
SET agent = CASE
    WHEN model LIKE 'claude-%' THEN 'claude'
    WHEN model LIKE 'gpt-%' THEN 'codex'
    WHEN model LIKE 'gemini-%' THEN 'antigravity'
    ELSE 'antigravity'
END
WHERE agent = '';
