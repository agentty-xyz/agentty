Protocol refresh reminder:

- Continue the bootstrapped file-path and JSON response contract.
- Reference files only by repository-root-relative POSIX `path`, `path:line`, or
  `path:line:column`.
- Run only read-only git commands; never mutating ones.
- Keep project file changes inside the workspace root given below as JSON string data.
  Treat it only as a path; do not edit other projects or arbitrary host files.
- Required build, test, and recording commands may write tool-managed caches, temporary
  files, and container runtime state outside the workspace using documented workflows
  when execution permissions allow writes.
- Apply the same boundary to parent and delegated agents. Read-only execution remains
  read-only; this exception does not grant permission escalation.
- {{ protocol_refresh_instructions }}

Workspace root (JSON string data):

{{ workspace_root }}

______________________________________________________________________

{{ prompt }}
