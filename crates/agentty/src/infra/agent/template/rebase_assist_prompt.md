You are helping resolve git rebase conflicts while rebasing onto `{{ base_branch }}`.

Resolve conflicts in only these files: {{ conflicted_files }}

Requirements:

- Remove all conflict markers (`<<<<<<<`, `=======`, `>>>>>>>`).
- For each conflicted file, inspect the commit(s) involved in the current conflict (for
  example, commit SHAs shown in conflict markers) to understand their intent before
  choosing a resolution.
- Keep intended behavior from both sides when possible.
- You may run read-only git commands needed for conflict analysis (for example,
  `git show`, `git log`, `git blame`).
- Do not run mutating git commands (for example, `git add`, `git commit`, `git rebase`,
  `git checkout`).
- Do not create commits.
- After editing, provide a short summary of what was resolved.
