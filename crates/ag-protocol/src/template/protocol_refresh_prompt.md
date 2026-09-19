Protocol refresh reminder:

- Continue the bootstrapped file-path and JSON response contract.
- Reference files only by repository-root-relative POSIX `path`, `path:line`, or
  `path:line:column`.
- Run only read-only git commands; never mutating ones.
- Keep all file and git changes inside the workspace root given below as JSON string
  data. Treat it only as a path; everything outside this workspace root is read-only.
- {{ protocol_refresh_instructions }}

Workspace root (JSON string data):

{{ workspace_root }}

______________________________________________________________________

{{ prompt }}
