# testty-cli

Language-agnostic command-line front end for the `testty` framework. Installs a `testty`
binary so non-Rust projects can drive TUI end-to-end scenarios without writing Rust
harness code.

## Status

Packaging skeleton. The full clap command tree and argument parsing are complete, but
every verb is stubbed: each prints "not yet implemented" to stderr and exits non-zero.
Later tasks replace the individual `Command::dispatch` arms with real behavior.

## Entry Points

- `src/main.rs`: Router-only `clap` `Cli` definition, the `Command` and `ProofCommand`
  trees, and `Command::dispatch`, which currently maps every verb to `not_implemented`.

## Verbs

- `run <scenario> [--bin <BIN>] [--proof <DIR>]`: execute a scenario file.
- `schema`: print the scenario JSON schema.
- `proof open <html>`: open a single proof report.
- `proof gallery <dir>`: build a gallery from proof reports.
- `update`: update stored scenario snapshots.

## How to Extend

1. Add or adjust the verb in the `Command` / `ProofCommand` enums in `src/main.rs`.
1. Add an arg-parsing test under `#[cfg(test)] mod tests` first (TDD).
1. Replace the matching `not_implemented` arm in `Command::dispatch` with real logic,
   delegating into the `testty` library crate.

## Change Guidance

- Keep verb behavior thin: parse and validate arguments here, delegate execution to the
  `testty` library crate.
- When verbs gain real behavior, update `README.md` and the framework docs under
  `docs/site/content/docs/`.
