-- A separate worktree a session keeps for one task's writes. The branch and
-- directory names derive from the session and worktree identifiers, so the
-- row records only what recreation and cleanup cannot derive: the commit the
-- worktree branches from and how far creation or removal got. Rows disappear
-- with their session; the host removes the directories before that.
CREATE TABLE task_worktree (
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    id TEXT NOT NULL CHECK (length(trim(id)) > 0),
    base_commit TEXT NOT NULL CHECK (length(trim(base_commit)) > 0),
    state TEXT NOT NULL CHECK (state IN ('creating', 'ready', 'removing')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, id)
);
