DELETE FROM session_message
WHERE position < (
    SELECT MAX(legacy_checkpoint.position)
    FROM session_message AS legacy_checkpoint
    WHERE legacy_checkpoint.session_id = session_message.session_id
      AND legacy_checkpoint.kind = 'legacy_transcript'
);

UPDATE session_message
SET kind = 'workflow_notice'
WHERE kind IN ('legacy_transcript', 'transcript_chunk');
