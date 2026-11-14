---
name: test-pyramid
description: Choose the cheapest test level that proves a behavior once, avoid duplicate coverage, and keep local and CI feedback fast when adding, changing, reviewing, or consolidating tests.
---

# Test Pyramid

Use this skill whenever a change adds, edits, reviews, or removes tests. It decides
where a behavior is proven; root `AGENTS.md` still defines which gates must pass, and
`crates/AGENTS.md` defines how test files are written.

## Principle

Prove each behavior once, at the lowest level that can observe it. A higher level may
assert only what lower levels cannot see. Line and function coverage is satisfied by the
lowest test that reaches the code, never by adding a slower test.

## Levels

Pick the first level that can observe the behavior. Costs are approximate per-test
magnitudes from local macOS runs; Linux CI is cheaper, so slow fixtures hurt local
iteration most.

1. **Pure unit** (default, ≈15 ms, the nextest per-process floor). Domain rules,
   parsers, formatting, prompt rendering, reducers, and key-to-intent mapping over mode
   state. No database, Git, temporary directory, subprocess, `App` startup, or real
   time. Build state directly, as `new_test_prompt_mode` does in
   `crates/agentty/src/runtime/mode/prompt_test/`, not as its `new_test_prompt_app`
   cases do (a full `App`, ≈100–200 ms each). When code takes `&mut App` but the
   behavior is a state transformation, extract a pure step (mode and input to new mode
   and intents) and test that.
1. **Boundary unit**. Workflow orchestration through injected traits with `mockall`
   mocks or scripted worker facilities. Add an in-memory database only when persisted
   state is the subject, and a real Git repository only when Git behavior is the
   subject.
1. **Crate contract** (`crates/*/tests/`). One test per public contract rule, using the
   crate's public API in process. Leave argument permutations and error branches to
   levels 1–2.
1. **CLI subprocess** (crate `tests/` that spawn the built binary, such as
   `crates/ag-harness-cli/tests/cli.rs`; ≈0.5–1 s). One test per command-line contract:
   arguments, exit status, and output streams. Keep parsing and branch logic below.
1. **PTY scenario** (`crates/agentty/tests/e2e/`, `FeatureTest`; ≈1–12 s). One scenario
   per user-visible journey, asserting rendering and wiring. Do not re-check ordering,
   validation, or error permutations already pinned below.

## Execution Policy

Test levels describe what a test proves. Local versus CI execution is a separate
decision: follow root `AGENTS.md` and the required procedure in
`docs/contributing/validation.md`. A manual gate is not another test level.

Gated suites hold only tests that need their environment; place everything else at one
of the levels. The native sandbox suite (`crates/ag-harness/tests/sandbox.rs`) runs in
its own serialized test group, `crates/agentty/tests/prompt_evaluation.rs` calls live
providers as an ignored manual check, and `showcase` and `protocol_compliance_e2e` run
with the PTY suite. Their hooks are defined in `.pre-commit-config.yaml`.

## Compile Cost

Test code compiles with its crate. Most of `crates/agentty/src/` (≈150k of ≈230k lines)
is test code in one lib-test unit that dominates the CI coverage build, and any test
edit there rebuilds it. Put logic and its tests in the smallest owning crate. Each file
in a crate's `tests/` is a separate binary with its own link; extend an existing
integration binary instead of adding a file.

## Before Adding a Test

1. Name the behavior in one sentence and find its owner level from the list above.
1. Search for existing coverage with `rg` over test names and asserted values. If a test
   at that level already proves the behavior, extend it with a case instead of adding a
   function.
1. If a higher-level test already covers the behavior, add the lower-level test and
   narrow the higher one to what only it can observe.
1. Express variations as a case table in one test rather than near-identical functions.
1. Reuse the lightest existing fixture. When none fits, add a light builder to the
   suite's support module instead of reaching for `new_test_app` or a Git repository.

## Fixture and Time Rules

- Never start an `App`, create a session, or initialize Git to test a transformation of
  in-memory state; one `new_test_app`, with or without the Git template, costs as much
  as about ten pure tests.
- Use the cached fixtures: `Database::in_memory` and `AppRepositories::in_memory` load a
  migrated-schema snapshot at little cost, and `setup_test_git_repo` in
  `crates/agentty/src/` copies a committed template repository. Keep
  `Database::open_in_memory`, which replays every migration (several times slower under
  coverage), for tests of migrations or production construction, and run Git commands
  directly only when Git behavior is the subject.
- Do not sleep on the wall clock to order events. Inject `Clock` or another time
  boundary; use `tokio::time::pause` where the code runs on Tokio time.
- Keep timeouts as failure guards only; a passing test must not wait for them. A passing
  test whose duration matches a timeout or interval, such as exactly 5 s, is waiting on
  a timer.
- Size large-input fixtures through injected limits instead of real megabytes. Coverage
  instrumentation multiplies per-byte work: an 8 MB input took ≈0.3 s locally and ≈18 s
  under coverage.
- In PTY scenarios, wait on content with `wait_for_text` or `eventually`, which return
  once the condition holds. `wait_for_stable_frame` always costs at least its
  `stable_ms`; reserve it for captured frames. `viewing_pause_ms` affects only VHS
  recordings, not PTY runs.
- Do not depend on host paths, users, or Git configuration; canonicalize expected paths
  and pin Git settings in fixtures.

## Consolidating Existing Tests

Consolidation removes duplicated proof, not failing evidence. It never replaces the root
rule against skipping or deleting a failing test during repair.

1. Pick a behavior cluster and list every test asserting it, across all levels.
1. Keep the lowest test that proves each distinct outcome; merge siblings into case
   tables; narrow higher-level tests to their unique assertions.
1. Delete a test only when another test at the same or a lower level asserts the same
   outcome. Name the superseding test in the change description.
1. Before deleting, confirm the superseding test reaches the same production lines; the
   `coverage` gate in CI must still pass.

## Fast Local Loop

- Use the focused and affected-package recipes in `docs/contributing/validation.md` for
  iteration and final confirmation; do not duplicate hook implementations here.
- For a focused PTY scenario, use the command in
  `docs/contributing/feature-test/authoring.md`, not the full E2E hook.

## Finding Slow Tests

`test-focused` writes per-test durations to `target/nextest/local/junit.xml`; CI hooks
use `target/nextest/ci/junit.xml`. Sort tests by time, group them by module, and treat
any level-1 or level-2 test over 0.5 s as a fixture problem first. CI keeps the same
report as an artifact for workspace, coverage, and E2E jobs.

## Review Checklist

- Is the behavior proven at its lowest observable level, and only once?
- Does the fixture build only what the assertion reads?
- Does a level-1 or level-2 test take over ≈100 ms locally? Review its fixture: tests
  between 100 ms and 0.5 s cost more in total than the few slower ones.
- Would a case table replace near-duplicate functions?
- Does any test wait on real time, a timer, or host-specific state?
- For new PTY scenarios: does each assertion concern rendering or wiring rather than
  logic already pinned below, and does each wait end on content?
