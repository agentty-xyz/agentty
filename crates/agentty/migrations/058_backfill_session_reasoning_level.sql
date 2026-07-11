UPDATE session
SET reasoning_level = COALESCE(
    (
        SELECT project_setting.value
        FROM project_setting
        WHERE project_setting.project_id = session.project_id
          AND project_setting.name = 'ReasoningLevel'
          AND project_setting.value IN ('low', 'medium', 'high', 'xhigh', 'max')
    ),
    'high'
)
WHERE reasoning_level IS NULL;
