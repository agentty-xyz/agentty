# Coverage and SonarQube

Pull-request CI owns fresh `coverage`. Run it locally only to diagnose its CI failure or
when requested:

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

## One Package's Gaps

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

A focused run rebuilds only the selected packages. Instrumented test binaries that
unselected dependents left in the coverage build directory still contain a changed
crate's old code, so the report can list gaps at line numbers that no longer match the
source. Add those dependents to `AGENTTY_TEST_PACKAGES` and keep execution narrow with
`AGENTTY_TEST_FILTER='package(=<package>)'` to rebuild them.

## `ag-harness` Feature Builds

`ag-harness` also builds without its default features. When a change touches its feature
gates, lint the crate with no features and with each optional feature that gates code
alone, then cover the builds without default features, with each provider family alone,
and with every provider family but no other backend; one merged report must cover every
line those builds compile:

```sh
prek run clippy-ag-harness-features --all-files --hook-stage manual
prek run coverage-ag-harness-features --all-files --hook-stage manual
```

`.cargo/config.toml` unifies features across the workspace, so a plain
`cargo check -p ag-harness --no-default-features` still builds every feature another
member enables. Set `CARGO_RESOLVER_FEATURE_UNIFICATION=selected` for a manual check;
both hooks do.

## Native Sandbox Qualification

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
