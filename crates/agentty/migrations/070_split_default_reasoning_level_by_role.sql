INSERT INTO project_setting (project_id, name, value)
SELECT project_id, 'DefaultSmartReasoningLevel', value
FROM project_setting
WHERE name = 'ReasoningLevel'
  AND value IN ('low', 'medium', 'high', 'xhigh', 'max')
ON CONFLICT(project_id, name) DO UPDATE SET value = excluded.value;

INSERT INTO project_setting (project_id, name, value)
SELECT project_id, 'DefaultFastReasoningLevel', value
FROM project_setting
WHERE name = 'ReasoningLevel'
  AND value IN ('low', 'medium', 'high', 'xhigh', 'max')
ON CONFLICT(project_id, name) DO UPDATE SET value = excluded.value;

INSERT INTO project_setting (project_id, name, value)
SELECT project_id, 'DefaultReviewReasoningLevel', value
FROM project_setting
WHERE name = 'ReasoningLevel'
  AND value IN ('low', 'medium', 'high', 'xhigh', 'max')
ON CONFLICT(project_id, name) DO UPDATE SET value = excluded.value;

INSERT INTO project_setting (project_id, name, value)
SELECT projects.id, roles.name, legacy_global.value
FROM project AS projects
CROSS JOIN setting AS legacy_global
CROSS JOIN (
    SELECT 'DefaultSmartReasoningLevel' AS name
    UNION ALL SELECT 'DefaultFastReasoningLevel'
    UNION ALL SELECT 'DefaultReviewReasoningLevel'
) AS roles
WHERE legacy_global.name = 'ReasoningLevel'
  AND legacy_global.value IN ('low', 'medium', 'high', 'xhigh', 'max')
  AND NOT EXISTS (
      SELECT 1
      FROM project_setting AS legacy_project
      WHERE legacy_project.project_id = projects.id
        AND legacy_project.name = 'ReasoningLevel'
  )
ON CONFLICT(project_id, name) DO NOTHING;

DELETE FROM project_setting WHERE name = 'ReasoningLevel';
DELETE FROM setting WHERE name = 'ReasoningLevel';
