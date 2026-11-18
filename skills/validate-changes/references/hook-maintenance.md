# Instruction and Hook Maintenance

## Instruction Checks

`check-instructions` validates instruction aliases, literal repository paths, local
Markdown link targets, and hook IDs used in `prek run` commands across `AGENTS.md`
files, `CONTRIBUTING.md`, and every Markdown file under `skills/`, and rejects
parent-relative paths in `AGENTS.md` files. It runs through
`ag-xtask check-instructions` on every default check invocation, including when
referenced files are moved or deleted. Its unit and public CLI tests cover valid and
broken instruction fixtures; validate checker changes with the affected-package recipe
in `skills/validate-changes/SKILL.md` for `ag-xtask`, whose CLI suites the coverage gate
includes.

## Test and Coverage Hooks

`crates/ag-xtask/tests/test_focused.rs` and `crates/ag-xtask/tests/coverage_hook.rs`
execute the cataloged `test-focused`, `coverage`, and `coverage-focused` commands with a
stub runner to verify selection, required inputs, arguments, stale-report removal,
isolation of coverage build settings from `check-coverage`, and failure propagation.
`crates/ag-xtask/tests/harness_feature_hooks.rs` checks that
`clippy-ag-harness-features` and `coverage-ag-harness-features` build every selection
without default features under selected feature unification, keep it and the coverage
build settings away from `check-coverage`, and propagate failures.

`coverage-ag-harness-sandbox` still lacks such regression coverage; configuration and
instruction checks are not replacements. Review changes to coverage commands, compiler
wrappers, and native sandbox setup for argument quoting, failure propagation, report
freshness, and setup ordering.
