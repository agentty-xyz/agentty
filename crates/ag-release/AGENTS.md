# ag-release

Release planning and packaging for Cargo workspaces published on GitHub.

## Invariants

- Keep sources project-agnostic: every project-specific value comes from
  `[workspace.metadata.release]` or `cargo metadata`. Name test fixtures neutrally.
- `crates/ag-release/README.md` is the public contract. Update it in the same change as
  any configuration key, command, plan JSON field, or artifact name.
- Derive artifact file names only in `crates/ag-release/src/artifact.rs`.
