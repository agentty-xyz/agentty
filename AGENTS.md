# Agentty

Agentty is a Rust workspace for an agent-management TUI and reusable support crates.

## Start Here

- Treat this root guide as the baseline and the nearest nested `AGENTS.md` as the local
  specialization.
- Read `skills/AGENTS.md` and use the smallest matching skill set when a request names a
  skill or clearly matches one.
- For external library, framework, SDK, API, CLI, or cloud-service details, query
  Context7 before answering or coding; fall back to official documentation only when
  Context7 is unavailable.
- On a non-`main` branch, inspect the complete diff from the fork point, including
  committed, uncommitted, and untracked changes, before deciding what changed.
- Before editing, identify affected tests, documentation, dependencies, dependents, and
  repository hooks.

## Product vs Repository Scope

Prompts about branches, worktrees, commits, reviews, sessions, or agent behavior usually
describe Agentty product requirements, not instructions to mutate this checkout.

- Apply workflow requirements to projects managed by Agentty unless the user explicitly
  names this repository, worktree, branch, or session.
- If local mutation still appears necessary after that interpretation, clarify the scope
  before acting.

## Non-Negotiable Gates

- Preserve unrelated user changes.
- Every user-visible feature requires an E2E test in `crates/agentty/tests/e2e/` built
  with `FeatureTest`. Use `skills/feature-test/SKILL.md`; if infrastructure blocks
  coverage, report the exact gap.
- Every code change requires automated tests that cover 100% of its coverable changed
  lines. Before handoff, run `prek run diff-coverage --all-files --hook-stage manual`
  and `prek run coverage --all-files --hook-stage manual`.
- Never bypass `prek`-managed hooks with `--no-verify`, `--no-gpg-sign`, or an
  equivalent flag. Fix the failure.
- Prefer removing legacy behavior. Obtain explicit user approval before retaining it.

## Rust Conventions

### Workspace and Modules

- Define all dependencies, including development and build dependencies, under
  `[workspace.dependencies]` in the root `Cargo.toml`. Member manifests use
  `workspace = true` for shared package metadata and dependencies.
- Use singular Rust file names. For nested modules, prefer `module.rs` plus `module/`;
  do not add `mod.rs`.
- A `module.rs` paired with `module/` is router-only: declarations and re-exports, with
  no runtime types, constants, functions, or implementations.

### Readability

- Order code for top-to-bottom reading: public before restricted before private, with
  callees ordered by first use.
- Keep imports at file scope. Prefer module-oriented internal imports, use direct item
  imports only when clearer, and do not mix imported-module and fully qualified styles.
  In tests, prefer `use super::*;`.
- Add `new()` or `Default` only for meaningful initialization. Prefer associated
  constructors over free construction helpers.
- Put an inherent `impl` directly below its struct and trait implementations after it.
  Keep helpers used by one type inside that type's `impl`.
- Put public struct fields before private fields and alphabetize within each group.
- Use descriptive names; avoid single-letter names and near-identical names in one
  scope.
- Separate logical blocks with blank lines, including before explicit or implicit
  returns except in a one-expression block.
- Introduce abstractions only for reuse, reduced complexity, or testability. Inline
  pass-through wrappers that add no behavior or boundary.
- Do not silence Clippy with `#[allow(...)]`; resolve the underlying issue.

### Tests and Boundaries

- Give every touched test explicit `// Arrange`, `// Act`, and `// Assert` sections;
  combine labels only when that improves a very small test.
- Keep test-only code inside `#[cfg(test)] mod tests` unless it belongs to an
  established shared test-support surface. Mockable traits may use
  `#[cfg_attr(test, mockall::automock)]`.
- Keep a real test for an isolated external command. For flows with multiple external
  calls, inject a trait boundary and use deterministic mocks.
- Reuse named fixtures, builders, and expectation helpers instead of duplicating test
  setup. Do not expose production APIs solely to share test fixtures.
- Route process, filesystem, network, terminal, and time access through injected
  boundaries in orchestration code.
- When removing behavior, test the remaining supported path rather than only asserting
  that the old path is absent.

## Tokio

- Keep the codebase async; do not create a runtime merely to call `block_on()`.
- Enable only required Tokio features, never `full`.
- Use `tokio::process::Command` for streamed subprocesses and
  `tokio::task::spawn_blocking` for blocking synchronous work.
- Prefer variable shadowing or a scoped block when cloning values into spawned `move`
  closures.
- Use `#[tokio::test]` for async tests and `tokio::time::sleep` for async delays.

### Mutex Selection

- Default to `std::sync::Mutex`; use `tokio::sync::Mutex` only when the protected
  critical section itself awaits.
- Never hold an async mutex merely to perform synchronous work such as writing to a
  `std::fs::File`.

## Quality Gates

`.pre-commit-config.yaml` is the executable source of truth for hook IDs and commands.
Invoke its hooks through `prek`; do not copy their underlying commands into other
workflows.

- While iterating, run the relevant formatter or fixer on touched paths.
- Before handoff, run one impact-based validation rung covering every touched file and
  all affected dependencies and dependents:
  - Markdown: `mdformat` and the default hooks for the touched paths.
  - Docs site: the Markdown checks plus `zola-check`; reformat touched templates with
    `djlint-reformat`.
  - Rust: `rustfmt-fix`, `cargo-check`, `clippy`, affected crate/dependent tests, and
    `coverage`.
  - Manifests, migrations, and the hook catalog: add their dedicated checks from
    `.pre-commit-config.yaml`.
- For cross-cutting changes or uncertain impact, run `prek run --all-files`, then
  `prek run test-workspace --all-files --hook-stage manual`.
- Run mutating fixers one at a time and inspect their diffs before continuing.
- Run focused Agentty E2E tests locally; CI owns the complete E2E suite.
- Kill and report any test that produces no output for five minutes. After three failed
  repair attempts, stop and report the test, output, and attempted fixes; never skip,
  ignore, or delete the test.

## Documentation

Apply the smallest documentation update matching the change:

- Keep documentation short and conceptual unless detailed implementation documentation
  is explicitly requested.
- Keep Rust doc comments current for touched public APIs and related elements.
- Update `docs/site/content/docs/` for user-visible behavior and
  `docs/site/content/docs/architecture/` for ownership, boundaries, or runtime flow.
- Update `README.md` for public prerequisites, usage, features, or crate information.
- Update `CHANGELOG.md` for shipped behavior during release work.
- In prose, wrap code identifiers, file names, key bindings, and configuration literals
  in backticks.

Follow the nearest documentation guide for exact routing and integrity rules.

## Git and Releases

- Use `skills/git-commit/SKILL.md` for commit preparation, commit messages, and
  pull-request descriptions.
- Use `skills/bump-version/SKILL.md` for release preparation. Local work stops at the
  ordinary version-bump change; create and publish no release tags locally.
- After that change lands, create its `v`-prefixed tag for the exact commit in the
  GitHub UI; the release workflows publish from that tag.
- Treat `.github/workflows/release.yml` as generated. Upgrade `cargo-dist` through
  `dist-workspace.toml` and regenerate with `dist init`; review both files together.

## Instruction Files

- Add or change an `AGENTS.md` only for durable scope-specific purpose, invariants,
  change routing, or documentation synchronization.
- Do not duplicate inherited rules, implementation inventories, or facts readily
  recoverable from manifests, module routers, tests, or CLI help.
- Do not use parent-directory-relative paths in an `AGENTS.md`.
- When creating an `AGENTS.md`, add same-directory `CLAUDE.md` and `GEMINI.md` symlinks
  targeting it.

## Canonical References

- `docs/site/content/docs/architecture/module-map.md`: ownership and layer boundaries.
- `docs/site/content/docs/architecture/runtime-flow.md`: orchestration and channels.
- `docs/site/content/docs/architecture/testability-boundaries.md`: external boundaries.
- `docs/site/content/docs/architecture/change-recipes.md`: safe change paths.
- Agent prompt templates live under `crates/ag-agent/src/agent/template/`,
  `crates/agentty/src/app/template/`, and `crates/ag-protocol/src/template/`.
