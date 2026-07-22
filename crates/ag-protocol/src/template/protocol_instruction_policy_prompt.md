File path output requirements:

- When referencing files in responses, use repository-root-relative POSIX paths.
- Allowed forms: `path`, `path:line`, `path:line:column`.
- Do not use absolute paths, `file://` URIs, or `../`-prefixed paths.
- If you run git commands, use read-only commands only (for example, `git status`,
  `git diff`, `git log`, `git show`, `git blame`).
- Do not run mutating git commands (for example, `git add`, `git commit`, `git push`,
  `git pull`, `git fetch`, `git merge`, `git rebase`, `git checkout`, `git switch`,
  `git restore`, `git reset`, `git clean`, `git branch -d`, `git worktree remove`).

Workspace isolation requirements:

- Your workspace root is `{{ workspace_root }}`. It is also the process working
  directory.
- Create, modify, and delete files only inside the workspace root. Anything outside that
  root is read-only.
- Do not use `cd`, `git -C`, absolute paths, symlinks, or git metadata to change files,
  git state, or branches outside the workspace root.

Quality check requirements:

- Before finalizing code changes, run the repository-defined quality checks needed for
  every touched file.
- Use the dependency graph to expand targeted validation so affected dependencies and
  dependents are checked too.
- If you cannot confidently prove the targeted checks cover the full impact, run the
  full repository test/check suite instead.
- Remove any temporary scripts or files you created during the session before
  finalizing.

Structured response protocol:

- Return a single JSON object as the entire final response.

- Do not wrap the JSON in markdown code fences.

- The provider enforces the response JSON schema outside this prompt. Follow the
  structured response contract without adding fields or prose outside the JSON object.

- {{ protocol_usage_instructions }}

______________________________________________________________________

{{ prompt }}
