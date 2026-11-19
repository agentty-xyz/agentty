-- Former stacked children target their root ancestor's base branch, because
-- the parent branch they targeted is deleted once the parent merges.
WITH RECURSIVE ancestry(session_id, ancestor_id, depth) AS (
    SELECT id, parent_session_id, 1
    FROM session
    WHERE parent_session_id IS NOT NULL
    UNION ALL
    SELECT ancestry.session_id, parent.parent_session_id, ancestry.depth + 1
    FROM ancestry
    JOIN session AS parent ON parent.id = ancestry.ancestor_id
    WHERE parent.parent_session_id IS NOT NULL AND ancestry.depth < 64
)
UPDATE session
SET base_branch = (
    SELECT root.base_branch
    FROM ancestry
    JOIN session AS root ON root.id = ancestry.ancestor_id
    WHERE ancestry.session_id = session.id
    ORDER BY ancestry.depth DESC
    LIMIT 1
)
WHERE parent_session_id IS NOT NULL
    AND status NOT IN ('Merged', 'Done', 'Canceled');

-- Merged former children keep the parent branch as their review-request
-- target, which main sync no longer finalizes. Their work already merged
-- upstream, so retarget the review request to the root base branch so the
-- next main sync marks them `Done` and cleans up their worktree.
WITH RECURSIVE ancestry(session_id, ancestor_id, depth) AS (
    SELECT id, parent_session_id, 1
    FROM session
    WHERE parent_session_id IS NOT NULL
    UNION ALL
    SELECT ancestry.session_id, parent.parent_session_id, ancestry.depth + 1
    FROM ancestry
    JOIN session AS parent ON parent.id = ancestry.ancestor_id
    WHERE parent.parent_session_id IS NOT NULL AND ancestry.depth < 64
)
UPDATE session_review_request
SET target_branch = (
    SELECT root.base_branch
    FROM ancestry
    JOIN session AS root ON root.id = ancestry.ancestor_id
    WHERE ancestry.session_id = session_review_request.session_id
    ORDER BY ancestry.depth DESC
    LIMIT 1
)
WHERE session_id IN (
    SELECT id
    FROM session
    WHERE parent_session_id IS NOT NULL AND status = 'Merged'
);

DROP INDEX IF EXISTS idx_session_parent_session_id;

ALTER TABLE session DROP COLUMN parent_session_id;
ALTER TABLE session DROP COLUMN stack_base_commit_hash;
