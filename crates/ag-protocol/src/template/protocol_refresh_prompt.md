Protocol refresh reminder:

- Continue following the previously bootstrapped file-path and JSON response contract.
- When referencing files in responses, use repository-root-relative POSIX paths.
- Allowed forms: `path`, `path:line`, `path:line:column`.
- If you run git commands, use read-only commands only.
- Do not run mutating git commands.
- Keep every file and git change inside the workspace root `{{ workspace_root }}`;
  anything outside that root is read-only.
- {{ protocol_refresh_instructions }}

______________________________________________________________________

{{ prompt }}
