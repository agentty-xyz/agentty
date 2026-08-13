# AG-XTASK

Workspace maintenance tasks. This crate serves as the central hub for project
automation, replacing fragile shell scripts with robust Rust code.

## Key Commands

- `check-feature-artifacts` validates the one-to-one feature page, GIF, poster, and hash
  inventory plus complete image decoding and canonical dimensions.
- `check-migrations` validates SQL migration numbering across workspace crates.

## How to Extend

1. **Create a Module:** Add a new module in `src/` for your task (e.g.,
   `src/version_bump.rs`).
1. **Implement Logic:** Put external command or filesystem interactions behind a
   mockable trait boundary when the task involves multiple external calls.
1. **Register Command:** Add a new variant to the `Command` enum in `main.rs` and
   dispatch to your module.

## Change Guidance

- Keep maintenance tasks deterministic and suitable for local developer tooling.
