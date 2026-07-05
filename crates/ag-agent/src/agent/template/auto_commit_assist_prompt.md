You are helping fix a failed git commit in an agent session worktree.

Commit error: {{ commit_error }}

Requirements:

- Fix only what is needed so a follow-up commit can succeed.
- Do not run mutating git commands. Read-only inspection commands are allowed when
  needed, such as `git status`, `git diff`, `git log`, or `git show`.
- Do not create commits.
- Keep changes minimal and preserve intended behavior.
- After editing, return the required protocol JSON object, put a short summary of what
  was fixed in `answer`, leave `questions` empty, and set `summary` to null.
