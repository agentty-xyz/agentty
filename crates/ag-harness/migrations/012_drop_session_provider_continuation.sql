ALTER TABLE session DROP COLUMN provider_session_id;

-- Recorded outcomes drop rejected native-resume requests, whose response type
-- no longer exists, so completed host requests stay readable.
UPDATE session_turn
SET terminal_outcome = json_set(
    terminal_outcome,
    '$.outcome.report.model_requests',
    json((
        SELECT json_group_array(json(request.value))
        FROM json_each(terminal_outcome, '$.outcome.report.model_requests') AS request
        WHERE json_extract(request.value, '$.response_type') <> 'ResumeUnavailable'
    ))
)
WHERE EXISTS (
    SELECT 1
    FROM json_each(terminal_outcome, '$.outcome.report.model_requests') AS request
    WHERE json_extract(request.value, '$.response_type') = 'ResumeUnavailable'
);
