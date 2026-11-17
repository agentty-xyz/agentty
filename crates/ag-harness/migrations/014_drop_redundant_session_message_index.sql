-- The unique (session_id, turn_position, message_position) constraint already
-- serves every lookup by this leading prefix.
DROP INDEX session_message_session_id_turn_position_idx;
