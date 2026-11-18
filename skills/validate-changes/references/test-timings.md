# Test Timings

## Reports and Artifacts

The focused hook writes test durations to `target/nextest/local/junit.xml`; CI hooks use
`target/nextest/ci/junit.xml`. Cargo writes compilation timings to the build directory's
`cargo-timings/cargo-timing.html`. CI retains both as artifacts for 14 days for
workspace, source, coverage, native sandbox, and E2E jobs, including after failures.
Artifact names include the run ID, attempt, and job; each `metadata.json` identifies the
source SHA, suite, runner environment, cache-hit status, hook outcome and time, and
report hashes. Only reports from the prepared invocation are accepted, and missing ones
stay explicit. Cancelled jobs may have no artifact.

## Compare Two Revisions

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

## Slow Tests

`ag-xtask slow-tests <junit.xml>` lists the slowest tests, per-binary and per-module
totals (`--module-depth`, `--top`), and a duration histogram for one report. After the
coverage step, CI runs it as `check-slow-tests` on `target/nextest/ci/junit.xml`: a test
over 5 s fails unless `.config/slow-tests.yaml` lists it with a reason. Allowlisted
tests that are now fast or gone only warn; remove their entries. The threshold leaves
headroom above the slowest unlisted coverage-run tests (about 3.3 s on loaded runners),
so it does not flake. The PTY E2E suite is not checked.

## Timing Collector

`test-timing-action` checks the collector's lifecycle and failure paths with 100%
JavaScript line coverage and writes a fresh `timing-action.lcov`; the coverage job runs
it before scanning so SonarCloud imports JavaScript coverage alongside Rust. Rust report
parsing and comparison are covered by the ordinary `ag-xtask` suites and coverage gate.
