-- Consolidate retired `gpt-5.4` usage rows into `gpt-5.5` before renaming
-- models in place. This avoids violating the `(session_id, model)` uniqueness
-- constraint when one session already has both `gpt-5.4` and `gpt-5.5` usage
-- rows.
INSERT INTO session_usage (session_id, model, created_at, input_tokens, invocation_count, output_tokens)
SELECT
    session_id,
    'gpt-5.5',
    MIN(created_at),
    SUM(input_tokens),
    SUM(invocation_count),
    SUM(output_tokens)
FROM session_usage
WHERE session_id IS NOT NULL
  AND model IN ('gpt-5.5', 'gpt-5.4')
GROUP BY session_id
HAVING SUM(CASE WHEN model = 'gpt-5.4' THEN 1 ELSE 0 END) > 0
ON CONFLICT(session_id, model) DO UPDATE SET
    created_at = excluded.created_at,
    input_tokens = excluded.input_tokens,
    invocation_count = excluded.invocation_count,
    output_tokens = excluded.output_tokens;

DELETE FROM session_usage
WHERE session_id IS NOT NULL
  AND model = 'gpt-5.4';

UPDATE session
SET model = 'gpt-5.5'
WHERE model = 'gpt-5.4';

UPDATE session_usage
SET model = 'gpt-5.5'
WHERE session_id IS NULL
  AND model = 'gpt-5.4';

UPDATE setting
SET value = 'gpt-5.5'
WHERE name IN ('DefaultSmartModel', 'DefaultFastModel', 'DefaultReviewModel')
  AND value = 'gpt-5.4';

UPDATE project_setting
SET value = 'gpt-5.5'
WHERE name IN ('DefaultSmartModel', 'DefaultFastModel', 'DefaultReviewModel')
  AND value = 'gpt-5.4';
