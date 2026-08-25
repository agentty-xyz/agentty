Resolve git rebase conflicts while rebasing onto `{{ base_branch }}`.

{{ workspace_note }}

Resolve conflicts in only these files:

{{ conflicted_files }}

Requirements:

- Remove every conflict marker (`<<<<<<<`, `=======`, `>>>>>>>`).
- Before choosing each resolution, inspect the commits involved in that conflict,
  including SHAs shown in markers, to understand their intent.
- Keep intended behavior from both sides when possible.
- Limit git inspection to read-only commands such as `git show`, `git log`, and
  `git blame`. Never run mutating commands such as `git add`, `git commit`,
  `git rebase`, or `git checkout`, and do not create commits.
- Run the repository-defined quality checks for the resolved files and their affected
  dependencies or dependents. If targeted coverage is unclear, run the full repository
  test/check suite.
- Return the required protocol JSON object. Briefly summarize the resolution in `answer`
  and leave `questions` empty.
