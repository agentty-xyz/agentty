# ag-harness-cli

Interactive command-line host for structured model turns and durable harness sessions.

## Boundaries

- Own CLI defaults, application prompts, credentials/configuration selection, and
  terminal-safe output here. Delegate model execution, persistence, and repository-tool
  enforcement to `ag-harness`.
- Derive provider parsing and help from the library's model configuration contracts.
  Keep provider wire formats and transport logic out of this crate.

## Integration

- Select the database location and repository at the host boundary. Preserve explicit
  opt-in for writes and comparison-base selection; do not widen tool permissions while
  translating CLI options.
- Resolve a trusted Git executable outside repository scope and pass the validated
  repository into the harness.
- Preserve terminal-safe rendering of model output and use process-level tests for
  changes to invocation, defaults, or interactive behavior.

## Documentation

Keep `crates/ag-harness-cli/README.md` aligned with CLI behavior. Refer to
`crates/ag-harness/README.md` for library integration and
`crates/ag-harness-cli/tests/cli.rs` for the process-level contract.
