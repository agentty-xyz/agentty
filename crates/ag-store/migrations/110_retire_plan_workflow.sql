-- The experimental /plan workflow was replaced by /graph. Its graph markers and
-- natural-language reply ledger are discarded. Planner and Implementer answers
-- stay visible as ordinary assistant answers, and saved graphs, tasks, and the
-- rest of each session remain intact.
UPDATE session_message
SET kind = 'assistant_answer',
    content = trim(
        json_extract(content, '$.text'),
        ' ' || char(9) || char(10) || char(13)
    )
WHERE kind = 'agent_answer'
  AND json_valid(content)
  AND json_type(content, '$.text') = 'text'
  AND trim(
        json_extract(content, '$.text'),
        ' ' || char(9) || char(10) || char(13)
    ) <> '';

DELETE FROM session_message
WHERE kind IN ('agent_answer', 'agent_graph');

DROP TABLE execution_graph_reply;
