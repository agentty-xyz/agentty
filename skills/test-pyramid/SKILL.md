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
assert only what lower levels cannot see. Changed-line coverage is satisfied by the
lowest test that reaches the line, never by adding a slower test.

## Levels

Pick the first level that can observe the behavior:

1. **Pure unit** (default). Domain rules, parsers, formatting, prompt rendering,
   reducers, and key-to-intent mapping over mode state. No database, Git, temporary
   directory, subprocess, `App` startup, or real time. Build state directly, as
   `new_test_prompt_mode` does in `crates/agentty/src/runtime/mode/prompt_test/`. Aim
   for under 10 ms.
1. **Boundary unit**. Workflow orchestration through injected traits with `mockall`
   mocks or scripted worker facilities. Add an in-memory database only when persisted
   state is the subject, and a real Git repository only when Git behavior is the
   subject. A full `App` with a session still costs about 0.1 to 1 s under parallel
   load.
1. **Crate contract** (`crates/*/tests/`). One test per public contract rule, using the
   crate's public API. Leave argument permutations and error branches to levels 1–2.
1. **PTY scenario** (`crates/agentty/tests/e2e/`, `FeatureTest`). One scenario per
   user-visible journey, asserting rendering and wiring. Do not re-check ordering,
   validation, or error permutations already pinned below; scenarios cost 2 to 30 s.
1. **Manual gates**. `coverage`, `test-workspace`, the full `test-agentty-e2e` suite,
   and `prompt-evaluation` run in CI or on request, as root `AGENTS.md` defines.

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
  in-memory state.
- Use the cached fixtures: `Database::in_memory` and `AppRepositories::in_memory` load a
  migrated-schema snapshot, and `setup_test_git_repo` in `crates/agentty/src/` copies a
  committed template repository. Keep `Database::open_in_memory` for tests of migrations
  or production construction, and run Git commands directly only when Git behavior is
  the subject.
- Do not sleep on the wall clock to order events. Inject `Clock` or another time
  boundary; use `tokio::time::pause` where the code runs on Tokio time.
- Keep timeouts as failure guards only; a passing test must not wait for them.
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

- Iterate with `test-focused` and a filter for the changed behavior, plus
  `AGENTTY_TEST_PACKAGES` when only some packages need compiling; see
  `docs/contributing/validation.md`.
- Run whole-package suites once for the final state. The `agentty` library suite alone
  runs for minutes, so do not use it as an inner loop.
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
- Would a case table replace near-duplicate functions?
- Does any test wait on real time or host-specific state?
- For new PTY scenarios: does each assertion concern rendering or wiring rather than
  logic already pinned below?
