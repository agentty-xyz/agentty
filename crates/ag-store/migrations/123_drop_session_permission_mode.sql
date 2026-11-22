-- Sessions no longer choose a permission mode: every chat turn edits as
-- Auto Edit, and only internal utility turns run read-only.
ALTER TABLE session DROP COLUMN permission_mode;
