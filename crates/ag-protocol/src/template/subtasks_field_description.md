Proposed child-session subtasks for an orchestrator planning turn. Emit at most {{
max_subtasks }} items, and use an empty array when the turn prompt did not ask for a
decomposition plan. Defaults to an empty array when omitted. Ordinary session turns and
utility prompts must always leave this empty; emitting subtasks there has no effect.
Every subtask runs unattended in its own worktree branched from the same base branch, so
each one must be independently completable. Use `touched_areas` as optional, best-effort
planning guidance: list literal repository-relative file or directory paths without
wildcard patterns when they are predictable. Areas may overlap, and workers may modify
additional files when required by their task. When the goal does not split into two or
more independent subtasks, emit an empty array and recommend a regular single session in
`answer` instead of proposing one ceremonial subtask.
