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
- Follow `crates/AGENTS.md` for Rust conventions, including workspace dependency rules
  when editing the root `Cargo.toml`.

## Product vs Repository Scope

- Resolve scope from the request and conversation context. Requests to review or fix
  this checkout authorize that work without requiring an explicit repository name.
- Apply requirements about Agentty-managed projects to the product. Ask for
  clarification only when product versus checkout scope remains ambiguous and would
  change the action; continue independent work while awaiting the answer.

## Model Execution Boundary

Every Agentty-managed action requiring model reasoning or generation must follow **Run
Worker → Agent Runtime → Harness → LLM**, including background and utility work. This
applies to every harness. Application workflows must not invoke harnesses or model APIs
directly, including for retries or nested calls.

Follow `docs/site/content/docs/core-components/execution.md` for the execution contract
and `crates/agentty/src/app/AGENTS.md` for workflow integration rules.

## Non-Negotiable Gates

- Preserve unrelated user changes.
- Agentty UI features demonstrable in a PTY scenario require `FeatureTest` E2E coverage
  under `crates/agentty/tests/e2e/`; follow `skills/feature-test/SKILL.md`. For behavior
  requiring live backends or unavailable infrastructure, report the exact coverage gap
  and test the supported boundaries deterministically.
- Use integration tests appropriate to the public surface for other CLI, library, or
  backend features.
- Every code change requires automated tests covering 100% of its coverable changed
  lines. Before handoff, run `prek run coverage --all-files --hook-stage manual` once
  for the final relevant state. This hook generates a fresh report and enforces both
  workspace and changed-line thresholds.
- Never bypass `prek`-managed hooks with `--no-verify`, `--no-gpg-sign`, or an
  equivalent flag. Fix the failure.
- Prefer removing obsolete behavior within the requested scope. Ask only when choosing
  between preserving an existing public contract and making a breaking change that the
  user has not already authorized; leave unrelated compatibility behavior alone.

## Quality Gates

`.pre-commit-config.yaml` is the executable source of truth for hook IDs and commands.
Invoke cataloged checks through `prek`. The focused E2E validation and container
recording commands in `skills/feature-test/SKILL.md` are explicit exceptions because the
E2E hook runs the complete suite. Keep those commands in the skill; do not duplicate
hook implementations elsewhere.

- While iterating, run the relevant formatter or fixer on touched paths and focused
  tests for the changed behavior. Use `test-focused` with an explicit
  `AGENTTY_TEST_FILTER`; see `CONTRIBUTING.md` for package and dependency selection.
- Before handoff, run one impact-based validation rung covering every touched file and
  all affected dependencies and dependents:
  - Markdown: `mdformat` and the default hooks for the touched paths.
  - Docs site: the Markdown checks plus `zola-check`; reformat touched templates with
    `djlint-reformat`.
  - Rust: `rustfmt-fix`, `cargo-check`, `clippy`, affected crate/dependent tests, and
    `coverage`.
  - Manifests, migrations, and the hook catalog: add their dedicated checks from
    `.pre-commit-config.yaml`.
- Run required checks once for the final relevant state. Reuse successful results while
  their source, tests, dependencies, toolchain, configuration, and relevant environment
  remain unchanged; rerun after invalidating edits, failures, or new evidence. Record
  the command, scope, and result so the handoff shows what was covered.
- Preserve public-contract integration tests when selecting affected suites. Source test
  hooks and coverage do not replace them; use `test-focused` without a test-name
  restriction for affected packages, dependencies, and dependents, or `test-workspace`.
- For cross-cutting changes or uncertain impact, run `prek run --all-files`, then
  `prek run test-workspace --all-files --hook-stage manual`.
- Run mutating fixers one at a time and inspect their diffs before continuing.
- If Rust code is added, modified, or deleted, run the full E2E suite once for the final
  relevant state before handoff:
  `prek run test-agentty-e2e --all-files --hook-stage manual`. Focused iteration does
  not replace this final gate or the full CI suites.
- Let the runner's configured timeouts govern individual tests. After five minutes
  without output, inspect build and process activity; terminate and report a confirmed
  stall rather than treating silence alone as failure. After three failed repair
  attempts, stop and report the test, output, and attempted fixes; never skip, ignore,
  or delete the test.

## Documentation

Apply the smallest documentation update matching the change:

- Keep documentation short and conceptual unless detailed implementation documentation
  is explicitly requested.
- Keep Rust doc comments current for touched public APIs and related elements.
- Update `docs/site/content/docs/` for user-visible behavior; route architecture changes
  using the canonical references below.
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

Update these architecture pages when their subject changes; keep file-level detail in
source routers and doc comments:

- `docs/site/content/docs/architecture/module-map.md`: ownership and layer boundaries.
- `docs/site/content/docs/architecture/runtime-flow.md`: orchestration and channels.
- `docs/site/content/docs/architecture/testability-boundaries.md`: external boundaries.
- `docs/site/content/docs/architecture/change-recipes.md`: contributor change paths.

Agent prompt templates live under `crates/ag-agent/src/agent/template/`,
`crates/agentty/src/app/template/`, and `crates/ag-protocol/src/template/`.
