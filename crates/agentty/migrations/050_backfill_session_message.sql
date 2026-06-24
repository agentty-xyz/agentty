INSERT INTO session_message (session_id, position, kind, content, created_at)
SELECT session.id,
       COALESCE(
           (
               SELECT MAX(existing_message.position) + 1
               FROM session_message AS existing_message
               WHERE existing_message.session_id = session.id
           ),
           0
       ),
       'legacy_transcript',
       session.output,
       session.created_at
FROM session
WHERE LENGTH(session.output) > 0
  AND NOT EXISTS (
      SELECT 1
      FROM session_message AS existing_legacy_message
      WHERE existing_legacy_message.session_id = session.id
        AND existing_legacy_message.kind = 'legacy_transcript'
        AND existing_legacy_message.content = session.output
  );
