Child-session subtasks proposed only for an orchestrator planning turn. Emit at most {{
max_subtasks }} items. Emit an empty array when no decomposition was requested. The
field defaults to an empty array when omitted. Ordinary session and utility turns must
leave it empty, and emitted subtasks there have no effect. Each subtask runs unattended
in its own worktree from the same base branch and must be independently completable. Set
`kind` to `research` for temporary, read-only investigation whose report informs a later
planning wave; otherwise use `implementation`. For implementation tasks, `touched_areas`
is optional, best-effort guidance: when predictable, list literal repository-relative
file or directory paths without wildcards. Areas may overlap, and workers may modify
other files as required. A focused research wave may contain one task. If implementation
work has fewer than two independent subtasks, return an empty array and recommend a
regular single session in `answer` instead of one ceremonial implementation subtask.
