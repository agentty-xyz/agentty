-- Session role discriminator. NULL means the ordinary worker role, so existing
-- rows keep their behavior without a backfill. Orchestrator sessions never
-- receive commits in their own worktree, which is why the role, not the
-- lifecycle status, gates diff and merge affordances.
ALTER TABLE session
ADD COLUMN role TEXT;
