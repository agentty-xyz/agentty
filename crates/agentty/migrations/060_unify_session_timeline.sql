ALTER TABLE session_message
ADD COLUMN turn_id INTEGER NOT NULL DEFAULT 0;

ALTER TABLE session_message
ADD COLUMN entry_key TEXT;

ALTER TABLE session_message
ADD COLUMN state TEXT NOT NULL DEFAULT 'resolved';

UPDATE session_message AS message
SET turn_id = (
    SELECT COUNT(*)
    FROM session_message AS prompt
    WHERE prompt.session_id = message.session_id
      AND prompt.kind = 'user_prompt'
      AND prompt.position <= message.position
);

INSERT INTO session_message (
    session_id, position, kind, content, created_at, turn_id, entry_key, state
)
SELECT session.id,
       COALESCE(MAX(message.position), -1) + 1,
       'turn_summary',
       session.summary,
       session.updated_at,
       COALESCE(MAX(message.turn_id), 0),
       'turn_summary:' || COALESCE(MAX(message.turn_id), 0),
       'resolved'
FROM session
LEFT JOIN session_message AS message ON message.session_id = session.id
WHERE session.summary IS NOT NULL
  AND TRIM(session.summary) <> ''
GROUP BY session.id;

INSERT INTO session_message (
    session_id, position, kind, content, created_at, turn_id, entry_key, state
)
SELECT session.id,
       COALESCE(MAX(message.position), -1) + 1,
       'focused_review',
       session.focused_review_text,
       session.updated_at,
       COALESCE(MAX(message.turn_id), 0),
       'focused_review:' || session.focused_review_diff_hash,
       'resolved'
FROM session
LEFT JOIN session_message AS message ON message.session_id = session.id
WHERE session.focused_review_text IS NOT NULL
  AND TRIM(session.focused_review_text) <> ''
  AND session.focused_review_diff_hash IS NOT NULL
GROUP BY session.id;

CREATE UNIQUE INDEX session_message_session_id_entry_key_idx
ON session_message (session_id, entry_key);

ALTER TABLE session DROP COLUMN summary;
ALTER TABLE session DROP COLUMN focused_review_text;
ALTER TABLE session DROP COLUMN focused_review_diff_hash;
