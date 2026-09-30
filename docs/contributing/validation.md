# Validation Recipes

Select required gates from the root `AGENTS.md` and invoke their definitions through
`prek`. Do not copy hook implementations into ad hoc commands.

## Focused and Affected-Package Tests

During iteration, select the behavior being changed:

```sh
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

`test-focused` requires a nonempty filter and fails when no tests match. Both it and
`test-workspace` retain public integration tests, excluding only targets selected by the
separate `test-agentty-e2e` gate. A filter narrows execution, not necessarily
compilation. Use `test-workspace` when impact is uncertain.

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

Hook-contract regression tests are intentionally absent. CI executes the real gates, but
a successful run does not verify rejection of stale reports after failed generation,
propagation of comparison failures, or literal handling of filters and base refs.
Removing the standalone suite accepts this regression-coverage gap; configuration and
instruction checks are not replacements. Review changes to coverage, focused-test
commands, compiler wrappers, and native sandbox setup for argument quoting, failure
propagation, report freshness, and setup ordering.

## TUI Snapshots

Use the `test-agentty-e2e` hook for the final suite. Set `TUI_TEST_UPDATE=1` on the
`prek run` invocation only when intentionally refreshing snapshot baselines. The focused
scenario and recording procedures are in `docs/contributing/feature-test/authoring.md`
and `docs/contributing/feature-test/recording.md`.
