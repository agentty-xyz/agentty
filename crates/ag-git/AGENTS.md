# ag-git

Reusable Git, worktree, synchronization, rebase, and squash-merge operations.

## Boundaries

- Keep Git subprocesses behind `GitClient` and its internal command boundary.
- Keep application workflow policy in callers; this crate owns reusable Git mechanics.
- Expose deterministic mocks through the `test-utils` feature when dependents need
  `GitClient` tests.
