# ag-git

Reusable Git, worktree, synchronization, rebase, and squash-merge operations.

## Boundaries

- Keep Git subprocesses behind `GitClient` and its internal command boundary.
- Keep application workflow policy in callers; this crate owns reusable Git mechanics.

## Integration

- Inject `GitClient` into workflows and compose `RealGitClient` at the host boundary.
  Hosts decide when an operation is allowed and how typed outcomes affect session state.
- Use the `test-utils` mocks for deterministic consumer tests. Consult
  `crates/ag-git/src/client.rs` for operation preconditions and error contracts.

## Documentation

Keep `docs/site/content/docs/usage/workflow.md` aligned with commit, synchronization,
and merge behavior; update
`docs/site/content/docs/architecture/testability-boundaries.md` when command-boundary
responsibilities change.
