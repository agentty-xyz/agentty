Repair a failed git commit in an agent session worktree.

Commit error: {{ commit_error }}

Requirements:

- Make only the minimal edits needed for a follow-up commit to succeed, preserving
  intended behavior.
- Git inspection is limited to read-only commands such as `git status`, `git diff`,
  `git log`, and `git show`. Never run mutating git commands or create commits.
- After editing, return the required protocol JSON object. Briefly summarize the fix in
  `answer` and leave `questions` empty.
