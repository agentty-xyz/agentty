# Validation Recipes

Select required gates from the root `AGENTS.md` and invoke their definitions through
`prek`; do not copy hook implementations into ad hoc commands.

## Required Validation Procedure

During iteration:

1. Run the relevant formatter or fixer on touched paths and inspect its diff. Run
   mutating fixers one at a time. A fixer that rewrites files reports `Failed`; that is
   its result, not an error, so review the diff instead of rerunning it. `rustfmt-fix`
   needs no `--files` list: it formats Rust files changed since the fork from
   `origin/main`, or the whole workspace when formatter configuration or the toolchain
   changes.
1. Catch compile errors with
   `prek run cargo-check --hook-stage manual --files <changed paths>`. It compile-checks
   the packages `clippy` would select for those paths, without linting.
1. Run focused tests for the changed behavior with `test-focused` and an explicit
   `AGENTTY_TEST_FILTER`.

Before handoff (the task's final state, not each reply in a multi-turn session), choose
one validation rung covering every touched file and all affected dependents:

| Impact                              | Required final checks                                                                                    |
| ----------------------------------- | -------------------------------------------------------------------------------------------------------- |
| Markdown                            | `mdformat` and default hooks for touched paths                                                           |
| Docs site                           | Markdown checks plus `zola-check`; format touched templates with `djlint-reformat`                       |
| Rust                                | `rustfmt-fix`, `clippy`, `ast-grep`, and unrestricted `test-focused` for changed packages and dependents |
| Manifests, migrations, hook catalog | Add their dedicated checks from `.pre-commit-config.yaml`                                                |
| ast-grep rules (`lint/`)            | `ast-grep` and `ast-grep-test`                                                                           |

Final Rust selection must retain public-contract integration tests; source-only hooks do
not replace them. Remove test-name restrictions for affected packages, or use
`test-workspace`. For cross-cutting changes, run `prek run --all-files` plus affected
package tests after touched paths pass formatting and focused tests.

### Diagnosing Failures and Long Checks

Let the runner's configured timeouts govern individual tests. Run long checks in the
foreground, redirect large output to a log, and read its summary after the command
exits; do not poll with repeated `tail`, `ps`, or sleep loops. After five minutes
without output, inspect build and process activity once; terminate and report a
confirmed stall, not silence alone.

Diagnose a CI failure with its failing hook alone, adding `--hook-stage manual` where
required, rather than rerunning every gate after each edit. After three failed repair
attempts, stop and report the test, output, and attempted fixes; never skip, ignore, or
delete the test.

## Focused and Affected-Package Tests

During iteration, select the behavior being changed:

```sh
AGENTTY_TEST_PACKAGES='ag-git' \
  AGENTTY_TEST_FILTER='package(=ag-git) and test(worktree)' \
  prek run test-focused --all-files --hook-stage manual
```

For final affected-package validation, drop the test-name restriction and select the
changed package with its dependents: `rdeps(=ag-git)` selects `ag-git` and every package
that depends on it. Dependencies are compiled but not retested, because their sources
did not change. Combine selections with `or` when several packages change:

```sh
AGENTTY_TEST_FILTER='rdeps(=ag-git)' \
  prek run test-focused --all-files --hook-stage manual
```

`AGENTTY_TEST_PACKAGES` optionally limits compilation to a whitespace-separated list of
Cargo package names; without it, the hook builds the workspace. Cargo still builds
dependencies the selected packages require. An execution filter does not select
compilation packages, so for final validation either list every changed package and
dependent, or omit the build limit and use the `rdeps` filter.

`test-focused` requires a nonempty filter and fails when no tests match. It and
`test-workspace` retain public integration tests. Only the Agentty PTY suites are left
out: their `test = false` manifest entries reserve them for the separate
`test-agentty-e2e` gate, so other runs do not compile them.

`test-focused` uses the `local` nextest profile, which inherits CI's resource groups,
timeouts, and retry policy but stops scheduling tests after the first failure and waits
for running tests to finish. CI hooks keep the exhaustive `ci` profile, so one failure
does not prevent collection of others. A failing local run never satisfies final
validation or creates a reusable success stamp.

## Coordinated Validation

For final Rust validation, the coordinator automatically selects changed packages and
their workspace dependents, then runs `clippy` and unrestricted `test-focused` through
`prek`:

```sh
prek run validate --all-files --hook-stage manual
```

Selection covers committed changes since the fork from `origin/main`, staged and
unstaged changes, deleted and renamed paths, and untracked files. Shared or unowned
paths conservatively select the whole workspace. A missing base or empty change set
fails; select packages explicitly for a clean checkout. The command prints the selected
packages, hooks, paths, and reason before running checks.

Preview that plan without running checks or writing reuse stamps:

```sh
cargo run --locked -q -p ag-xtask -- validate --plan
```

Use `--base` with an existing ref to compare against a different branch. Explicit
`--packages` or `AGENTTY_TEST_PACKAGES` replaces automatic path selection; dependents
are still included:

```sh
AGENTTY_TEST_PACKAGES='ag-git ag-agent' \
  prek run validate --all-files --hook-stage manual
```

The coordinator passes the affected selection to its Clippy and `cargo-check`
subprocesses in `AGENTTY_CLIPPY_PACKAGES`, so the hook's full file inventory cannot
widen it. Ordinary `clippy` invocations select owners and dependents from their changed
files, and root configuration changes select the whole workspace. `clippy` also lints
Agentty's explicit PTY targets, with unchanged feature selection, when the dependency
graph (including development dependencies) says Agentty is affected, or when shared
files or an unspecified scope apply. The manual `clippy-agentty-e2e` hook runs just
those targets for diagnosis; default validation already includes them through `clippy`.

Concurrent coordinator runs wait on an OS lock in Cargo's effective target directory;
direct Cargo commands use only Cargo's own build lock. Check subprocesses clear `SKIP`
and `PREK_SKIP`, so requested checks always execute. Keep target directories inside the
checkout and free of tracked source files, with downloaded registry and compiler caches
in their default shared locations; target artifacts are excluded from input
fingerprints.

Direct `ag-xtask validate` invocation accepts ordered `--hook` IDs and optional
`--reuse`; fresh execution is the default. Reuse applies only to allowlisted
deterministic hooks whose package selection, tracked and untracked source, Git state and
configuration (only the checked-out branch's `branch.<name>.*` section), tool versions,
and environment all match. Changes or failures invalidate success, and inputs changing
during execution cannot produce a successful stamp. Coverage always runs fresh.
Repository-required hooks still apply.

## Test Timings

The focused hook writes test durations to `target/nextest/local/junit.xml`; CI hooks use
`target/nextest/ci/junit.xml`. Cargo writes compilation timings to the build directory's
`cargo-timings/cargo-timing.html`. CI retains both as artifacts for 14 days for
workspace, source, coverage, native sandbox, and E2E jobs, including after failures.
Artifact names include the run ID, attempt, and job; each `metadata.json` identifies the
source SHA, suite, runner environment, cache-hit status, hook outcome and time, and
report hashes. Only reports from the prepared invocation are accepted, and missing ones
stay explicit. Cancelled jobs may have no artifact.

Download at least three samples per revision and compare matching suites and
environments:

```sh
cargo run --locked -q -p ag-xtask -- compare-timings \
  --before evidence/before/*/metadata.json \
  --after evidence/after/*/metadata.json
```

The JSON comparison reports medians, ranges, and percentage changes separately for hook,
compilation, execution, and summed test-process time. It rejects failed, incomplete,
duplicate, corrupt, or mixed-environment samples. Keep rejected samples in the
experiment record and report failures alongside successful timings; do not select only
favorable runs. Warm up both revisions equally and retain raw artifacts. A percentage
alone does not establish a reliable improvement when ranges overlap. Summed process time
is work across concurrent tests, not elapsed time.

Interpret the metadata conservatively. `exact_cache_hit: false` may mean a partial
restore or a miss, and `null` means unknown; neither proves a cold build. Hook time
excludes setup and queueing, which come from Actions job and step timestamps, and can
include scheduling between the collector steps. Cargo's rounded timing report cannot
resolve subsecond warm-build differences. E2E caches remain isolated by their pinned
container and Rust environment.

Compare timings from the same final head and completed run attempt. A new PR push
cancels that PR's obsolete Presubmit and Native Harness Sandbox runs, and a new `main`
push cancels an obsolete Native Harness Sandbox run on `main`; cancelled samples measure
superseded work, not suite speed. Postsubmit, merge-group, and manual runs remain
independent. Default-setup CodeQL runs are managed separately.

`ag-xtask slow-tests <junit.xml>` lists the slowest tests, per-binary and per-module
totals (`--module-depth`, `--top`), and a duration histogram for one report. After the
coverage step, CI runs it as `check-slow-tests` on `target/nextest/ci/junit.xml`: a test
over 5 s fails unless `.config/slow-tests.yaml` lists it with a reason. Allowlisted
tests that are now fast or gone only warn; remove their entries. The threshold leaves
headroom above the slowest unlisted coverage-run tests (about 3.3 s on loaded runners),
so it does not flake. The PTY E2E suite is not checked.

`test-timing-action` checks the collector's lifecycle and failure paths with 100%
JavaScript line coverage and writes a fresh `timing-action.lcov`; the coverage job runs
it before scanning so SonarCloud imports JavaScript coverage alongside Rust. Rust report
parsing and comparison are covered by the ordinary `ag-xtask` suites and coverage gate.

## Coverage

```sh
prek run coverage --all-files --hook-stage manual
```

The hook runs the `test-workspace` selection, so CI does not run that suite separately.
It generates a fresh `coverage.lcov`, then `ag-xtask check-coverage` fails unless every
coverable production line and function is covered. Failed or empty generation fails the
gate; an old report cannot satisfy it. CI retains `coverage.lcov` as an artifact, even
after a failed gate, so platform-specific gaps can be checked locally.

The gap report lists each file's uncovered line ranges and functions. A file record
without `DA` lines or any `FNF`, `FNH`, `LF`, or `LH` total fails.

To work on one package's gaps, run its tests with coverage and check only its sources:

```sh
AGENTTY_TEST_PACKAGES='ag-git' AGENTTY_COVERAGE_PATHS='crates/ag-git' \
  prek run coverage-focused --all-files --hook-stage manual
```

`coverage-focused` requires `AGENTTY_TEST_PACKAGES`. Optional `AGENTTY_TEST_FILTER`
narrows execution with a nextest filterset, and optional whitespace-separated
`AGENTTY_COVERAGE_PATHS` limits the check to repository-relative files or directories;
otherwise every reported file is checked. The hook writes a fresh
`target/coverage-focused.lcov` with the fail-fast `local` profile. Code reached only
through other packages' tests appears uncovered there; the `coverage` gate remains
authoritative.

`ag-harness` also builds without its default features. When a change touches its feature
gates, run the minimal-build leg, which lints the crate with no features and with each
optional feature alone, then requires full coverage of its sources from the tests that
build without default features:

```sh
prek run coverage-ag-harness-no-default-features --all-files --hook-stage manual
```

A focused run rebuilds only the selected packages. Instrumented test binaries that
unselected dependents left in the coverage build directory still contain a changed
crate's old code, so the report can list gaps at line numbers that no longer match the
source. Add those dependents to `AGENTTY_TEST_PACKAGES` and keep execution narrow with
`AGENTTY_TEST_FILTER='package(=<package>)'` to rebuild them.

Native Harness Sandbox qualifies only the macOS launcher, and pull requests run it only
when they touch `ag-harness`, its workspace dependencies, or shared build and CI
configuration. `crates/ag-xtask/tests/harness_sandbox_filter.rs` fails when the filter's
crate list drifts from Cargo's dependency graph. The coverage job installs Bubblewrap
and runs every `ag-harness` sandbox suite, so it is the Linux qualification.

## SonarQube Issues

Pull-request CI fails on any open SonarQube issue in the changed code. The built-in
quality gate grades new code by rating and can pass a pull request that still adds
issues, so the coverage job waits for the gate, reads the analysis's `new_violations`
count, and annotates each issue. Fix the reported code. Add a rule exception to
`sonar-project.properties` only with a comment explaining why the rule does not apply.
Pull requests from forks are not scanned. `crates/ag-xtask/tests/sonar_issue_gate.rs`
runs that step with a stub `curl` and saved responses.

## Instruction and Hook Maintenance

`check-instructions` validates instruction aliases, literal repository paths, local
Markdown link targets, and hook IDs used in `prek run` commands. It runs through
`ag-xtask check-instructions` on every default check invocation, including when
referenced files are moved or deleted. Its unit and public CLI tests cover valid and
broken instruction fixtures; validate checker changes with the affected-package recipe
for `ag-xtask`, whose CLI suites the coverage gate includes.

`crates/ag-xtask/tests/test_focused.rs` and `crates/ag-xtask/tests/coverage_hook.rs`
execute the cataloged `test-focused`, `coverage`, and `coverage-focused` commands with a
stub runner to verify selection, required inputs, arguments, stale-report removal,
isolation of coverage build settings from `check-coverage`, and failure propagation.
`coverage-ag-harness-sandbox` and `coverage-ag-harness-no-default-features` still lack
such regression coverage; configuration and instruction checks are not replacements.
Review changes to coverage commands, compiler wrappers, and native sandbox setup for
argument quoting, failure propagation, report freshness, and setup ordering.

## TUI Snapshots

Use the `test-agentty-e2e` hook for the final suite. Set `TUI_TEST_UPDATE=1` on the
`prek run` invocation only when intentionally refreshing snapshot baselines. The focused
scenario and recording procedures are in `docs/contributing/feature-test/authoring.md`
and `docs/contributing/feature-test/recording.md`.
