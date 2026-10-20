# Agentty

Agentty is a Rust workspace for an agent-management TUI and reusable support crates.

## Start Here

- Read this root guide and each ancestor `AGENTS.md` for the paths you will inspect or
  change. Nested guides add local rules; they do not replace inherited requirements.
  Load unrelated guides and skill references only when the task needs them.
- Read `skills/AGENTS.md` and use the smallest matching skill set when a request names a
  skill or clearly matches one.
- For external library, framework, SDK, API, CLI, or cloud-service details, query
  Context7 before answering or coding; fall back to official documentation only when
  Context7 is unavailable. Documentation lookup is permitted during inspection-only
  reviews; reuse already-fetched documentation when the version and question still
  match.
- On a non-`main` branch, inspect the complete diff from the fork point, including
  committed, uncommitted, and untracked changes, before deciding what changed.
- Before editing, identify affected tests, documentation, dependencies, dependents, and
  repository hooks.
- Follow `crates/AGENTS.md` for Rust conventions, including workspace dependency rules
  when editing the root `Cargo.toml`.
- Edit files with the harness's edit or patch tool. Do not write ad hoc scripts, such as
  Python heredocs, to edit sources or inspect processes.

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
  under `crates/agentty/tests/e2e/`; follow `crates/agentty/tests/e2e/AGENTS.md`. For
  behavior requiring live backends or unavailable infrastructure, report the exact
  coverage gap and test the supported boundaries deterministically.
- When changes affect published feature captures or recording settings, use
  `skills/refresh-feature-recordings/SKILL.md`. Refresh each affected GIF, hash sidecar,
  and PNG poster together; passing behavioral assertions alone does not establish
  artifact freshness.
- Use integration tests appropriate to the public surface for other CLI, library, or
  backend features.
- Every code change requires automated tests covering 100% of its coverable changed
  lines. CI enforces this with the `coverage` hook, which generates a fresh report and
  checks workspace and changed-line thresholds.
- Never bypass `prek`-managed hooks with `--no-verify`, `--no-gpg-sign`, or an
  equivalent flag. Fix the failure.
- Prefer removing obsolete behavior within the requested scope. Ask only when choosing
  between preserving an existing public contract and making a breaking change that the
  user has not already authorized; leave unrelated compatibility behavior alone.

## Quality Gates

`.pre-commit-config.yaml` owns hook IDs and executable definitions. Invoke checks
through `prek`. Focused E2E and container recording commands in
`docs/contributing/feature-test/` are explicit exceptions; keep those commands there.

- Follow the required iteration, handoff, and failure-diagnosis procedure in
  `docs/contributing/validation.md`; use `docs/contributing/setup.md` for setup. Format
  touched paths before focused tests. Final validation covers every touched file,
  changed package, affected dependent, and public contract. Unchanged dependencies need
  no retest.
- Run required checks once for the final relevant state. Reuse success only while
  source, tests, dependencies, toolchain, configuration, and relevant environment remain
  unchanged; rerun after invalidating edits, failures, or new evidence. Record each
  command, scope, and result at handoff.
- Pull-request CI owns `cargo-check`, fresh `coverage` (including the `test-workspace`
  selection), and full `test-agentty-e2e`. Run these locally only to diagnose their CI
  failure or when requested; also run `test-workspace` when impact cannot be bounded.
  Diagnose the single failing hook.
- Run hooks in the inherited environment. Do not override `CARGO_HOME`, `PREK_HOME`,
  `RUSTC_WRAPPER`, `TMPDIR`, or `CARGO_TARGET_DIR`, or copy registries, toolchains, or
  caches into the worktree. Report environment failures instead of building a
  replacement environment.
- Before container work, read `docs/contributing/feature-test/recording.md`. Use only
  cataloged hooks and commands in `docs/contributing/feature-test/`; do not compose ad
  hoc container invocations, images, or wrappers.

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
- The `check-instructions` hook checks literal repository paths, local Markdown links,
  instruction aliases, and hook names in `prek run` examples. Keep its regression tests
  aligned when changing the instruction format.

## Canonical References

Update these architecture pages when their subject changes; keep file-level detail in
source routers and doc comments:

- `docs/site/content/docs/architecture/module-map.md`: ownership and layer boundaries.
- `docs/site/content/docs/architecture/runtime-flow.md`: orchestration and channels.
- `docs/site/content/docs/architecture/testability-boundaries.md`: external boundaries.
- `docs/site/content/docs/architecture/change-recipes.md`: contributor change paths.

Prompt changes follow `docs/contributing/prompts.md` for ownership routing, contract
checks, and behavioral evaluation.
