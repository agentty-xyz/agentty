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
- Read `design/DESIGN.md` before changing how a web page or the terminal palette looks.
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

Follow
`docs/site/content/docs/core-components/execution-components/execution-boundary.md` for
the execution contract and `crates/agentty/src/app/AGENTS.md` for workflow integration
rules.

## Non-Negotiable Gates

`standards.toml` exports the shared gates to downstream workspaces; the Agentty gates
apply only here.

### Shared Gates

- Preserve unrelated user changes.
- Use integration tests appropriate to the public surface for CLI, library, or backend
  features that no more specific gate covers.
- Every coverable line and function of production Rust code stays at 100% automated test
  coverage. Never add coverage ignore patterns or service-side exclusions to satisfy it.
- Prefer removing obsolete behavior within the requested scope. Ask only when choosing
  between preserving an existing public contract and making a breaking change that the
  user has not already authorized; leave unrelated compatibility behavior alone.

### Agentty Gates

- Agentty UI features demonstrable in a PTY scenario require `FeatureTest` E2E coverage
  under `crates/agentty/tests/e2e/`; follow `crates/agentty/tests/e2e/AGENTS.md`. For
  behavior requiring live backends or unavailable infrastructure, report the exact
  coverage gap and test the supported boundaries deterministically.
- When changes affect published feature captures or recording settings, use
  `skills/refresh-feature-recordings/SKILL.md`. Refresh each affected GIF, hash sidecar,
  and PNG poster together; passing behavioral assertions alone does not establish
  artifact freshness.
- CI enforces the coverage gate with the `coverage` hook, which generates a fresh report
  and lists every gap; `coverage-focused` checks one package locally.

## Quality Gates

`.pre-commit-config.yaml` owns hook IDs and executable definitions. Invoke checks
through `prek`. Focused E2E and container recording commands in
`skills/write-feature-tests/` and `skills/refresh-feature-recordings/` are explicit
exceptions; keep those commands there.

- Follow the required iteration, handoff, and failure-diagnosis procedure in
  `skills/validate-changes/SKILL.md`; use `skills/setup-development/SKILL.md` for setup.
  Format touched paths before focused tests. Final validation covers every touched file,
  changed package, affected dependent, and public contract. Unchanged dependencies need
  no retest.
- Pull-request CI owns fresh `coverage` (including the `test-workspace` selection) and
  full `test-agentty-e2e`. Run these locally only to diagnose their CI failure or when
  requested; also run `test-workspace` when impact cannot be bounded. Diagnose the
  single failing hook.
- Run hooks in the inherited environment. Do not override `CARGO_HOME`, `PREK_HOME`,
  `RUSTC_WRAPPER`, `TMPDIR`, or `CARGO_TARGET_DIR`, or copy registries, toolchains, or
  caches into the worktree. Report environment failures instead of building a
  replacement environment.
- Render browser screenshots through Playwright, Puppeteer, or `chrome-headless-shell`.
  Launch a full Chrome or Chromium binary directly only with `--use-mock-keychain` and
  `--password-store=basic`; otherwise each macOS launch requests the user's login
  keychain password.
- Before container work, read
  `skills/refresh-feature-recordings/references/recording.md`. Use only cataloged hooks
  and the commands in that file and `skills/maintain-e2e-image/SKILL.md`; do not compose
  ad hoc container invocations, images, or wrappers.

## Documentation

Apply the smallest documentation update matching the change:

- Keep documentation short and conceptual unless detailed implementation documentation
  is explicitly requested.
- Keep Rust doc comments current for touched public APIs and related elements.
- Update `docs/site/content/docs/` for user-visible behavior; route architecture changes
  using the canonical references below.
- Keep the root `README.md` concise. Update it only for public prerequisites, usage,
  features, or crate information a new reader needs, in a sentence or short bullet. Put
  the details in `docs/site/content/docs/` and link to them instead of repeating them.
- Update `CHANGELOG.md` for shipped behavior during release work.
- In prose, wrap code identifiers, file names, key bindings, and configuration literals
  in backticks.

Follow the nearest documentation guide for exact routing and integrity rules.

## Git and Releases

- The repository is public. Keep commit messages, PR descriptions, and review comments
  free of sensitive information: secrets, credentials, personal data such as names,
  emails, or usernames, home-directory paths, private or session links, and details
  about individual users from telemetry. Describe affected users by role instead.
- When a task includes merging a pull request that the agent's authenticated GitHub
  account (`gh api user --jq .login`) authored, and that account is a code owner in
  `.github/CODEOWNERS` on `origin/main`, the agent may bypass the missing approval
  without asking again. Everything else still holds: every required check succeeded on
  the head commit, with none pending or missing, and no requested changes or unresolved
  review threads remain. Any other agent leaves merging to a maintainer.
- Use `skills/bump-version/SKILL.md` for release preparation. Local work stops at the
  ordinary version-bump change; create and publish no release tags locally.
- After that change lands, create its `v`-prefixed tag for the exact commit in the
  GitHub UI; the release workflows publish from that tag.
- `.github/workflows/release.yml` drives releases through `crates/ag-release`, which
  generates it. Never edit it by hand: change `[workspace.metadata.release]` or that
  crate and regenerate it with `cargo run -p ag-release -- workflow`. Its `plan` job is
  a required check on `main`, so it must keep that name and keep running on every pull
  request and merge-queue check.

## Standalone Harnesses

Agentty sessions leave these steps to Agentty's own workflows and prompts. Follow them
whenever you, or a harness such as Claude Code or Codex, run checks, Git, pull request,
or review steps directly in a checkout.

- Never bypass `prek`-managed hooks with `--no-verify`, `--no-gpg-sign`, or an
  equivalent flag. Fix the failure.
- Run required checks once for the final relevant state. Reuse success only while
  source, tests, dependencies, toolchain, configuration, and relevant environment remain
  unchanged; rerun after invalidating edits, failures, or new evidence. Record each
  command, scope, and result at handoff.
- Bring a branch up to date by rebasing it onto `origin/main` and pushing with
  `--force-with-lease`; never merge `main` into it.
- Fill `.github/pull_request_template.md` for PR descriptions. Its HTML comments are
  writing guidance: follow them, but never copy them into commit messages or PR
  descriptions.
- Branch each pull request from `main` unless the work splits into vertical slices, each
  fully usable on its own and depending on the one below it. Stack only those slices,
  and link them as a GitHub stack with the `gh stack` extension (`github/gh-stack`)
  instead of leaving separate pull requests with hand-set bases. Open each layer with
  its own filled template (`gh pr create --base <parent branch>`), then run
  `gh stack link` with the pull request numbers from bottom to top. `link` records no
  local stack, so run `gh stack checkout <pull request>` before any other `gh stack`
  command. If the extension is missing, say so and ask for it; do not leave the stack
  unlinked silently.
- Merge a stack from the top so the whole stack enters the merge queue at once:
  `gh stack merge <top pull request> --yes`. A bare number resolves to a stack number
  before a pull request number, so check that it names the intended pull request. Stacks
  cannot bypass merge requirements, so the approval bypass above covers single pull
  requests only; an unapproved stack waits for a maintainer's approval.
- After committing and before opening a pull request, run Agentty's focused review on
  `git diff <base branch>...HEAD`, such as `origin/main` or a stack layer's parent:
  follow `crates/agentty/src/app/template/review_assist_prompt.md` with the criteria
  `crates/agentty/src/app/review_rules.rs` selects for the changed paths and the
  conversation as session history, in a separate read-only agent when the harness offers
  one. Handle its suggestions as `crates/ag-session/src/template/apply_review_prompt.md`
  describes, and repeat for at most three rounds while actionable suggestions remain.
  Review again before marking it ready or merging it if that diff changed since the
  review; a rebase that leaves it unchanged needs no new review.
- Remove a worktree once the pull request for its branch has merged and nothing is
  working in it. Merged means `gh pr view <branch> --json state` reads `MERGED`: a pull
  request waiting in the merge queue is still open, and a squash merge leaves the branch
  out of `git branch --merged`. Remove the ones this session created; when the task
  includes merging, wait for the queue to land the pull request first, and otherwise
  name the worktree at handoff as pending removal. Before handoff, check
  `git worktree list` for other merged leftovers and list them for the user; remove
  those only after the user confirms they are idle, because a clean, unlocked worktree
  can still belong to a running session. Leave Agentty session worktrees, on `wt/<id>`
  branches, to Agentty. Run `git worktree remove <path>` without `--force`: a refusal
  means uncommitted changes or a lock, so leave that worktree in place and report it.
  Every worktree keeps its own build output, so leftovers hold tens of gigabytes.

## Instruction Files

### Writing Instructions

- Add or change an `AGENTS.md` only for durable scope-specific purpose, invariants,
  change routing, or documentation synchronization.
- Do not duplicate inherited rules, implementation inventories, or facts readily
  recoverable from manifests, module routers, tests, or CLI help.
- Do not restate a rule that Clippy or a repository hook already enforces.

### Instruction Checks

- The `check-instructions` hook checks literal repository paths, local Markdown links,
  parent-relative paths in `AGENTS.md`, instruction aliases, and hook names in
  `prek run` examples. Keep its regression tests aligned when changing the instruction
  format.
- `standards.toml` exports instruction sections by heading. Put workspace-specific
  guidance in a section it does not export; the `check-standards` hook rejects exported
  text that names this workspace's crates, hooks, or unexported paths.

## Canonical References

Update these architecture pages when their subject changes; keep file-level detail in
source routers and doc comments:

- `docs/site/content/docs/architecture/module-map.md`: ownership and layer boundaries.
- `docs/site/content/docs/architecture/runtime-flow.md`: orchestration and channels.
- `docs/site/content/docs/architecture/testability-boundaries.md`: external boundaries.
- `docs/site/content/docs/architecture/change-recipes.md`: contributor change paths.

Prompt changes follow `skills/evaluate-prompts/SKILL.md` for ownership routing, contract
checks, and behavioral evaluation.
