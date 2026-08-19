# Workspace Crates

`crates/` contains the Rust workspace members. Treat manifests and module roots as the
source of truth for the member and module inventory.

When adding or removing a crate required by a published workspace crate, update
`.github/workflows/publish-crates-io.yml` in the same change and keep dependencies
before dependents in the publish plan.
