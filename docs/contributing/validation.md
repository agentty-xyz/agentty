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

For final affected-package validation, run the changed package and its dependents
without a test-name restriction. `rdeps(=ag-git)` selects `ag-git` and every package
that depends on it. Dependencies are compiled but not retested, because their sources
did not change. Replace `ag-git` with the changed package, combining selections with
`or` when several packages change:

```sh
AGENTTY_TEST_FILTER='rdeps(=ag-git)' \
  prek run test-focused --all-files --hook-stage manual
```

`AGENTTY_TEST_PACKAGES` optionally limits compilation to a whitespace-separated list of
Cargo package names. Without it, the hook builds the workspace. Cargo still builds
dependencies required by the selected packages. For final validation, select every
changed package and dependent, or omit the build limit and use the `rdeps` filter above.
An execution filter does not select compilation packages.

`test-focused` requires a nonempty filter and fails when no tests match. Both it and
`test-workspace` retain public integration tests. Only the Agentty PTY suites are left
out: their `test = false` manifest entries reserve them for the separate
`test-agentty-e2e` gate, so other runs do not compile them. A filter narrows execution,
not necessarily compilation. Use `test-workspace` when impact is uncertain.

## Coordinated Validation

To expand changed packages through their workspace dependents, use the cataloged
coordinator:

```sh
AGENTTY_TEST_PACKAGES='ag-git ag-agent' \
  prek run validate --all-files --hook-stage manual
```

The coordinator executes `clippy` and unrestricted `test-focused` selections through
`prek`. Its Clippy subprocess receives the affected package selection in
`AGENTTY_CLIPPY_PACKAGES`, so the hook's full file inventory cannot widen that
selection. Ordinary Clippy hook invocations continue to select owners and dependents
from their changed files, with root configuration changes selecting the whole workspace.
The coordinator holds an OS lock in Cargo's effective target directory, resolved from
metadata and pinned for each check. Cargo configuration and target-directory environment
overrides are honored. The check subprocesses clear `SKIP` and `PREK_SKIP` so requested
checks execute before their successes are recorded. Other invocations of the coordinator
using that directory wait; direct Cargo commands still use Cargo's own build lock. Keep
target directories inside the checkout, with downloaded registry and compiler caches
left in their default shared locations. The target must not contain tracked source
files. Target artifacts, including paths linked into that directory, are excluded from
input fingerprints even when the target is not Git-ignored.

Direct `ag-xtask validate` invocation accepts `--packages`, ordered `--hook` IDs, and
optional `--reuse`. Fresh execution is the default. Reuse applies only to allowlisted
deterministic hooks with matching package selection, tracked and untracked source, Git
state/configuration, tool versions, and environment. Changes or failures invalidate
success; inputs changing during execution cannot produce a successful stamp. Coverage
always runs fresh. Repository-required hooks still apply.

## Test Timings

Nextest hooks write test execution durations to `target/nextest/ci/junit.xml` and Cargo
compilation timings to the build directory's `cargo-timings/cargo-timing.html`. CI
retains both as artifacts for 14 days for workspace, source, coverage, native sandbox,
and E2E jobs, including reports available after failures. Each artifact name includes
the run ID, attempt, and job. Its `metadata.json` identifies the source SHA, suite,
runner image, toolchain, container, exact cache-hit flag, hook outcome, elapsed hook
time, and report hashes. Preparation deletes old reports after cache restoration;
finalization accepts reports only from that prepared invocation. Missing preparation or
reports remain explicit. Cancelled jobs may have no artifact at all.

Download at least three samples per revision and compare matching suites and
environments:

```sh
cargo run --locked -q -p ag-xtask -- compare-timings \
  --before evidence/before/*/metadata.json \
  --after evidence/after/*/metadata.json
```

The JSON comparison reports medians, ranges, and percentage changes separately for hook,
compilation, execution, and summed test-process time. It rejects failed, incomplete,
duplicate, corrupt, or mixed-environment samples. Keep those rejected samples in the
experiment record and report failures alongside successful timings; do not select only
favorable runs. Warm up both revisions equally and retain raw artifacts. A percentage
alone does not establish a reliable improvement when ranges overlap. Summed process time
is work across concurrent tests, not elapsed time.

`exact_cache_hit: false` may mean a partial restore or a miss; `null` means unknown.
Neither proves a cold build. Hook time excludes setup and queueing; obtain those phases
from Actions job/step timestamps. The hook interval can include scheduling between the
collector steps. Cargo's rounded timing report cannot resolve subsecond warm-build
differences. E2E caches remain isolated by their pinned container and Rust environment.
The collector's lifecycle and failure paths are checked by `test-timing-action`,
including 100% JavaScript line coverage; Rust report parsing and comparison are covered
by the ordinary `ag-xtask` suites and coverage gate.

## Coverage

```sh
prek run coverage --all-files --hook-stage manual
```

Pull-request CI runs this gate; run it locally to diagnose a failure. The hook runs the
`test-workspace` selection, so CI does not run that suite separately. It generates a
fresh `coverage.lcov` and enforces workspace ratchets and complete coverage of changed
coverable Rust lines, including untracked files. The comparison defaults to local
`main`; set `AGENTTY_COVERAGE_BASE` for another existing base ref. CI selects the pull
request or merge group's remote base branch, falling back to the repository's default
branch. A missing base or failed generation fails the gate; an old report cannot satisfy
it.

Source-only tests do not replace public integration tests. Reuse results and diagnose
stalled runners according to `AGENTS.md`.

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
