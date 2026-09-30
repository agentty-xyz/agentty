# Validation Recipes

Select required gates from the root `AGENTS.md` and invoke their definitions through
`prek`. Do not copy hook implementations into ad hoc commands.

## Focused and Affected-Package Tests

During iteration, select the behavior being changed:

```sh
AGENTTY_TEST_PACKAGES='ag-git' \
  AGENTTY_TEST_FILTER='package(=ag-git) and test(worktree)' \
  prek run test-focused --all-files --hook-stage manual
```

For final affected-package validation, include dependencies and dependents without a
test-name restriction. Replace `ag-git` with the affected package, combining selections
when several packages change:

```sh
AGENTTY_TEST_FILTER='package(=ag-git) or deps(=ag-git) or rdeps(=ag-git)' \
  prek run test-focused --all-files --hook-stage manual
```

`AGENTTY_TEST_PACKAGES` optionally limits compilation to a whitespace-separated list of
Cargo package names. Without it, the hook builds the workspace. Cargo still builds
dependencies required by the selected packages. For final validation, select every
affected package, dependency, and dependent, or omit the build limit and use the
dependency-graph filter above. An execution filter does not select compilation packages.

`test-focused` requires a nonempty filter and fails when no tests match. Both it and
`test-workspace` retain public integration tests, excluding only targets selected by the
separate `test-agentty-e2e` gate. A filter narrows execution, not necessarily
compilation. Use `test-workspace` when impact is uncertain.

## Test Timings

Nextest hooks write test execution durations to `target/nextest/ci/junit.xml` and Cargo
compilation timings to the build directory's `cargo-timings/cargo-timing.html`. CI
retains both as artifacts for 14 days for workspace, source, coverage, native sandbox,
and E2E jobs, including reports available after failures. Compare compilation and
execution separately, and distinguish cold builds from cache restores before tuning
concurrency or partitioning suites. E2E builds use a cache isolated by the pinned
container and Rust environment.

## Coverage

```sh
prek run coverage --all-files --hook-stage manual
```

Pull-request CI runs this gate; run it locally to diagnose a failure. The hook generates
a fresh `coverage.lcov` and enforces workspace ratchets and complete coverage of changed
coverable Rust lines, including untracked files. The comparison defaults to local
`main`; set `AGENTTY_COVERAGE_BASE` for another existing base ref. CI selects the pull
request or merge group's remote base branch, falling back to the repository's default
branch. A missing base or failed generation fails the gate; an old report cannot satisfy
it.

Source-only tests and coverage do not replace public integration tests. Reuse results
and diagnose stalled runners according to `AGENTS.md`.

## Instruction and Hook Maintenance

`check-instructions` validates instruction aliases, literal repository paths, local
Markdown link targets, and hook IDs used in `prek run` commands. It runs on every
default check invocation, including when referenced files are moved or deleted, through
the `ag-xtask check-instructions` Rust command.

The `ag-xtask` unit and public CLI tests cover valid and broken instruction fixtures.
Use the affected-package recipe above with `ag-xtask`; the standard Rust coverage gate
includes its CLI suites to cover dispatch and process exit paths as well as the checker.

`crates/ag-xtask/tests/test_focused.rs` executes the cataloged focused-test command with
a stub runner to verify build selection, literal filters, required inputs, and failure
propagation. Coverage-hook rejection of stale reports, comparison failures, and literal
base refs still lacks regression coverage. Configuration and instruction checks are not
replacements. Review changes to coverage commands, compiler wrappers, and native sandbox
setup for argument quoting, failure propagation, report freshness, and setup ordering.

## TUI Snapshots

Use the `test-agentty-e2e` hook for the final suite. Set `TUI_TEST_UPDATE=1` on the
`prek run` invocation only when intentionally refreshing snapshot baselines. The focused
scenario and recording procedures are in `docs/contributing/feature-test/authoring.md`
and `docs/contributing/feature-test/recording.md`.
