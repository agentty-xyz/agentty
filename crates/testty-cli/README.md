# testty-cli

Language-agnostic command-line front end for the
[`testty`](https://crates.io/crates/testty) TUI end-to-end testing framework.

The CLI installs a `testty` binary that lets any project — not just Rust crates — drive
TUI scenarios, generate schemas, and inspect proof artifacts.

## Status

This crate is currently a packaging skeleton. The full command tree is in place, but
every verb is stubbed: it prints a "not yet implemented" notice and exits with a
non-zero status. Later work fills in the behavior.

## Commands

```text
testty run <scenario.yaml> [--bin <BIN>] [--proof <DIR>]
testty schema
testty proof open <html>
testty proof gallery <dir>
testty update
```

- `run` — execute a scenario file against a TUI binary.
- `schema` — print the JSON schema for scenario files.
- `proof open` — open a single proof report.
- `proof gallery` — build a gallery from a directory of proof reports.
- `update` — update stored scenario snapshots to match current output.
