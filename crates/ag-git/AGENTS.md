# AG-Git

Workspace library crate for git, worktree, sync, rebase, and squash-merge orchestration.

## Entry Points

- `src/lib.rs` is the public crate root.
- `src/client.rs` owns the `GitClient` boundary and production implementation.
- `src/repo.rs` owns low-level git command execution helpers.
- `src/sync.rs` owns commit, diff, push, pull, fetch, and tracking workflows.
- `src/rebase.rs` owns rebase and conflict workflows.
- `src/worktree.rs` owns worktree and branch-detection workflows.
- `src/merge.rs` owns squash-merge workflows.
- `src/error.rs` owns the shared typed error.

## Change Guidance

- Keep subprocess execution behind the existing git command boundary.
- Keep higher-level app workflow decisions in callers; this crate owns reusable git
  operations only.
- Expose test mocks through the `test-utils` feature when callers need deterministic
  `GitClient` tests.
