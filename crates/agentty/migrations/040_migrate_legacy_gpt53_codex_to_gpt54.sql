UPDATE session
SET model = 'gpt-5.4'
WHERE model IN ('gpt-5.3-codex', 'gpt-5.2-codex');

UPDATE session_usage
SET model = 'gpt-5.4'
WHERE model IN ('gpt-5.3-codex', 'gpt-5.2-codex');

UPDATE setting
SET value = 'gpt-5.4'
WHERE name IN ('DefaultSmartModel', 'DefaultFastModel', 'DefaultReviewModel')
  AND value IN ('gpt-5.3-codex', 'gpt-5.2-codex');

UPDATE project_setting
SET value = 'gpt-5.4'
WHERE name IN ('DefaultSmartModel', 'DefaultFastModel', 'DefaultReviewModel')
  AND value IN ('gpt-5.3-codex', 'gpt-5.2-codex');
