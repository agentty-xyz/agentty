Proposed child-session subtasks for an orchestrator planning turn. Emit at most {{
max_subtasks }} items, and use an empty array when the turn prompt did not ask for a
decomposition plan. Defaults to an empty array when omitted. Ordinary session turns and
utility prompts must always leave this empty; emitting subtasks there has no effect.
Every subtask runs unattended in its own worktree branched from the same base branch, so
each one must be independently completable and the `touched_areas` of any two subtasks
must be literal repository-relative file or directory paths without wildcard patterns
and must not overlap. When the goal does not split into two or more file-disjoint
subtasks, emit an empty array and recommend a regular single session in `answer` instead
of proposing one ceremonial subtask.
