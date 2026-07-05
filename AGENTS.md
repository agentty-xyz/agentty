# Agentty

TUI tool to manage agents.

## Project Facts

- Project is a Rust workspace; the `crates/` directory contains all workspace members.
- Workspace crate package names are `agentty`, `testty`, and any shared support crates
  that use the `ag-` prefix (for example, `ag-xtask`).
- `agentty`: A binary crate providing the CLI interface using Ratatui.
- `testty`: A library crate providing the Rust-native TUI end-to-end testing framework.
- **Workflow**: Agents are run in isolated git worktrees. Agentty creates these
  worktrees automatically for sessions launched from a git repository; treat that as
  product behavior, not as an instruction to mutate the local development checkout. Keep
  worktree lifecycle details aligned with
  `docs/site/content/docs/getting-started/overview.md` and
  `docs/site/content/docs/usage/workflow.md`.
- **Review**: Users review changes using the Diff view (`d` key in chat) which shows the
  output of `git diff` in the session's worktree.
- **Output**: Agent `stdout` and `stderr` are captured in parallel using `tokio` tasks
  to ensure prompts and errors are visible.

## Command Quick Reference

Hook IDs come from `.pre-commit-config.yaml`, the executable source of truth for
validation commands. See Quality Gates for when to run what.

- Format Rust sources: `prek run rustfmt-fix --files <paths> --hook-stage manual`
- Compile check: `prek run cargo-check --files <paths>`
- Lint: `prek run clippy --files <paths>`
- Test one crate: `prek run test-agentty-src` (also `test-ag-agent-src`,
  `test-ag-git-src`, `test-testty-src`, `test-ag-forge-src`, `test-ag-protocol-src`,
  `test-ag-clipboard-src`, `test-ag-xtask-src`)
- Focused E2E test:
  `cargo nextest run --locked --profile ci -p agentty --test e2e <test-filter>`
- Format markdown: `prek run mdformat --files <paths>`
- Full validation: `prek run --all-files`, then
  `prek run test-workspace --all-files --hook-stage manual`

# MANDATORY

> These rules are absolute and take precedence over all others.

- **Product-vs-Repository Scope:** Workflow prompts describe Agentty product behavior by
  default, not mutations of this checkout. Apply the rules in "Product-vs-Repository
  Prompt Scope" before acting on any workflow prompt.
- **Feature Test Gate:** Every user-visible feature must ship with a corresponding E2E
  feature test in `crates/agentty/tests/e2e/` using the `FeatureTest` builder from
  `common.rs`. If infrastructure blocks the feature test, report the blocker and the
  missing coverage in the handoff.
- **Never Bypass Hooks:** Do not use `--no-verify`, `--no-gpg-sign`, or any other flag
  that skips or disables git hooks managed by `prek`. If a hook fails, investigate and
  fix the underlying issue instead of bypassing it.
- **Legacy Retention Approval:** Prefer removing legacy code or behavior during
  development. If retaining legacy code or behavior for any reason, obtain explicit user
  approval first.
- **Preserve User Changes:** Never revert unrelated work unless explicitly asked.

## Task Start Checklist

1. Resolve whether the request targets Agentty product behavior or this local repository
   (see MANDATORY and "Product-vs-Repository Prompt Scope").
1. Read the nearest applicable `AGENTS.md`; apply this root guide as the baseline and
   the nearest guide as the local specialization. If the current directory does not have
   one, fall back to the closest ancestor guide and the architecture docs in
   `docs/site/content/docs/architecture/`.
1. Read `skills/AGENTS.md` and activate the smallest matching skill set when the task
   names a skill or matches a skill description.
1. If the request involves external library, framework, SDK, API, CLI, or cloud-service
   details, and Context7 is connected as an MCP server, query Context7 before answering
   or coding. If Context7 is unavailable, use official docs as the fallback and note the
   fallback in your response.
1. On a non-`main` branch, when a user asks for something that is not currently present
   on `main`, inspect the full worktree diff against the branch base/fork point before
   concluding what changed or remains. Include committed changes, uncommitted changes,
   and untracked files; avoid counting commits already applied to the base branch.
1. Before editing, identify any docs-sync targets, feature-test requirements, dependency
   impact, and validation hooks for the files likely to change.

## Product-vs-Repository Prompt Scope

Agentty is developed with Agentty, so prompts often describe developer workflows that
should become Agentty product behavior rather than instructions to operate on this
repository's current session.

- Treat workflow prompts about branches, worktrees, commits, reviews, sessions, agent
  prompts, or agent behavior as requirements for Agentty's user-facing functionality by
  default.
- Apply those prompts to the projects that Agentty manages for its users, not to the
  local Agentty development checkout, unless the user explicitly names this repository,
  this worktree, or the current branch/session.
- If a prompt could require a mutating action against the local repository, first
  reinterpret it as a product requirement. Ask for clarification only when the requested
  scope still remains ambiguous after that reinterpretation.
- Example: `make sure main branch is always up-to-date` means Agentty should provide
  behavior that keeps users' project `main` branches current for managed workflows; it
  does not mean this repository's local `main` branch should be updated.

## Rust Project Style Guide

- **Dependency Management:** ALL dependencies (including `dev-dependencies` and
  `build-dependencies`) must be defined in the root `Cargo.toml` under
  `[workspace.dependencies]`.
- All workspace crates must use `workspace = true` for shared package metadata and
  dependencies. Never define a version number inside a crate's `Cargo.toml`.
- **Release Profile:** Maintain optimized release settings in `Cargo.toml`
  (`codegen-units=1`, `lto=true`, `opt-level="s"`, `strip=true`) to minimize binary
  size.
- Use `ratatui` for terminal UI development.
- **Constructors:** Only add `new()` and `Default` when there is actual initialization
  logic or fields with meaningful defaults. For unit structs or zero-field structs,
  construct directly (e.g., `MyStruct`) — do not add boilerplate `new()` / `Default`
  impls.
- **Constructors:** Prefer `Type::new(...)` associated constructors over standalone
  helper functions when constructing that type.
- **Function Ordering:** Order functions to allow reading from top to bottom (caller
  before callee):
  - Public functions first.
  - Followed by less public functions (e.g., `pub(crate)`).
  - Private functions last.
  - If a function has multiple callees, they should appear in the order they are first
    called within that function.
- **File Naming:** Use **singular** names for Rust source files (e.g., `model.rs`,
  `icon.rs`, `agent.rs`). Do not use plural forms.
- **Module Layout:** Prefer `module.rs` paired with `module/` for modules that have
  child modules. Do not introduce new `mod.rs` files.
- **Parent Module Router Rule:** For every `module.rs` file paired with a `module/`
  directory, keep `module.rs` router-only. It may declare child modules and re-export
  child APIs, but must not define runtime logic, functions, structs, enums, traits, impl
  blocks, or constants.
- **Imports:** Always place imports at the top of the file. Do not use local `use`
  statements within functions or other blocks.
  - For internal crate paths, prefer module-oriented imports and namespace usage by
    default (for example, `use crate::infra::agent;` and `agent::create_backend(...)`).
  - Use direct item imports only when they materially improve readability (for example,
    frequently used traits/types like `AppMode`, `PathBuf`, `Arc`, or small focused
    groups in braces).
  - Do not mix styles for the same module in one file. If a module is imported, use that
    module alias/path consistently instead of also using fully-qualified `crate::...`
    paths.
  - Use fully-qualified `crate::...` references only when needed for disambiguation,
    explicit UFCS trait calls, or rustdoc links.
  - In test modules, prefer `use super::*;` where practical.
- **Test Coverage:** Cover all touched behavior with automated tests when practical.
  Critical logic always needs regression coverage; boilerplate or untestable I/O can use
  a pragmatic exception.
- **Test Isolation for External Commands:** Keep isolated single-command tests real when
  they validate one external command call, but for higher-level flows that involve
  multiple external command calls, always extract trait boundaries and mock them with
  `mockall` (`#[cfg_attr(test, mockall::automock)]`) to reduce runtime and flakiness.
- **Test-only code placement:** Never introduce `#[cfg(test)]` in production code
  outside `#[cfg(test)] mod tests` unless the code belongs to an established shared
  test-support surface for broadly reused fixtures. Prefer local helpers for one-off
  setup, but keep canonical fixtures such as session builders, deterministic clocks, and
  app client bundles in shared test support when duplication would make model or state
  changes mechanical. The other exception is `#[cfg_attr(test, mockall::automock)]` on
  traits used for mocking.
- **Reusable Test Surfaces:** When multiple tests need the same setup, row conversion,
  command expectation, or fixture data, extract a named helper or builder in the nearest
  test module or established test-support crate instead of duplicating setup. If the
  helper also removes production duplication, such as shared row metadata mapping or a
  bound status-transition context, make it production code with doc comments and test it
  directly. Keep assertion-only adapters and fixtures inside `#[cfg(test)] mod tests`;
  do not expose `pub(crate)` test-only APIs from production modules solely to share
  fixtures between tests.
- **Struct Fields:** Order fields in structs as follows:
  - Public fields first.
  - Private fields second.
  - Within each group, sort fields alphabetically.
- **Clippy Compliance:** Do not bypass clippy rules with `#[allow()]`. Adopt the
  solution that complies with the rule.
- **Code Grouping:** Within functions, separate related code blocks with empty lines.
  Group lines that belong together logically and add blank lines between distinct
  groups.
- **Return Spacing:** Always add an empty line before return statements, both explicit
  (`return`) and implicit (last expression). Exception: single-line blocks where the
  return is the only statement.
- **Impl Placement:** Place each standalone/inherent `impl StructName { ... }` block
  immediately below its `struct` declaration, then place trait impls (e.g.,
  `impl Trait for StructName`) after it.
- **Helper Placement:** Place helper functions used by only one struct inside that
  struct’s `impl`; keep only shared helpers at file scope.
- **Testability via DI:** Prefer trait-based dependency injection for external
  boundaries (terminal events, git/process calls, clocks/timers, filesystem, network) so
  logic can be tested deterministically.
  - Keep runtime wiring in production implementations and inject trait objects/generics
    into orchestration functions.
  - Use `#[cfg_attr(test, mockall::automock)]` on internal traits where mocking is
    needed.
  - Prefer testing behavior through injected fakes/mocks over end-to-end
    terminal/process dependencies when unit coverage is the goal.

## Database Standards (SQLx + SQLite)

### 1. Stack & Pattern

- **Driver:** `sqlx` (Feature: `sqlite`).
- **Runtime:** `tokio`.
- **Pattern:** Repository pattern or direct service-layer queries. **No ORM**.
- **Safety:** Prefer compile-time checked macros (`query!`, `query_as!`).
  - *Requirement:* `.sqlx` directory must be committed for offline compilation (CI/CD).
- **Concurrency:** Must enable **WAL Mode** (Write-Ahead Logging) for concurrent
  readers/writers.

### 2. Naming Conventions (Strict)

- **Tables:** `snake_case`, **SINGULAR** (e.g., `user`, `order_item`).
  - *Rationale:* Matches Rust struct names exactly (`User` -> `user`).
- **Columns:** `snake_case`.
  - **PK:** `id` (`INTEGER PRIMARY KEY AUTOINCREMENT`).
  - **FK:** `{table}_id` (e.g., `user_id`).
  - **Booleans:** Prefix with `is_`, `has_` (Stored as `INTEGER`, mapped to `bool`).
  - **Timestamps:** `{action}_at` (Stored as `INTEGER` (Unix) or `TEXT` (ISO8601)).
- **Rust Structs:**
  - Name: Singular, PascalCase (e.g., `User`).
  - Fields: `snake_case` (Matches DB columns 1:1).

### 3. Implementation Guidelines

1. **Configuration:**
   - Set `PRAGMA foreign_keys = ON;` (SQLite defaults to OFF).
   - Set `PRAGMA journal_mode = WAL;` (Crucial for performance).
1. **Migrations:** Embedded at compile time via `sqlx::migrate!()`.
   - Place SQL files in `crates/<crate>/migrations/` named `NNN_description.sql`.
   - Migrations run automatically on database open; no external CLI required.
   - Never modify existing migration files. Always add a new migration file for every
     schema change.
   - If `SQLite` cannot alter a structure in place (for example, changing a primary
     key), use a new migration that drops and recreates the table.
1. **Dependency Injection:** Pass `&sqlx::SqlitePool` to functions.
   - *Note:* SQLite handles cloning the pool cheaply.
1. **Error Handling:** Map `sqlx::Error` to domain-specific errors.

## Async Runtime (Tokio)

The project uses `tokio` as its async runtime. The binary entry point uses
`#[tokio::main]` and all I/O-bound operations are async.

### Feature Selection

- **NEVER** use `features = ["full"]`. The project optimizes for binary size — only
  enable the specific features you need.
- When adding a new tokio API, check which feature flag it requires and add only that
  flag.

### Mutex Selection: `std::sync::Mutex` vs `tokio::sync::Mutex`

- **Default to `std::sync::Mutex`** unless you need to hold the lock across an `.await`
  point.
- `tokio::sync::Mutex` is only needed when the critical section itself contains `.await`
  calls (e.g., async file I/O, async network calls).
- If the critical section is purely synchronous (e.g., `writeln!` to a `std::fs::File`,
  pushing to a `String`), use `std::sync::Mutex` even inside async functions. It is
  cheaper and avoids unnecessary async overhead.
- **Wrong:** `Arc<tokio::sync::Mutex<std::fs::File>>` with `file.lock().await` followed
  by sync `writeln!`.
- **Right:** `Arc<std::sync::Mutex<std::fs::File>>` with `file.lock().ok()` followed by
  sync `writeln!`.

### Blocking Operations

- Use `tokio::task::spawn_blocking` for operations that block the thread (e.g., shelling
  out to `git` via `std::process::Command`).
- Do **not** call blocking functions directly in async contexts — it starves the tokio
  worker threads.
- For subprocess management where you need async streaming of stdout/stderr, use
  `tokio::process::Command` instead.

### Variable Cloning for `move` Closures

- When cloning variables for `spawn_blocking` or `tokio::spawn` closures, prefer
  **variable shadowing** or **scoped blocks** over `_clone` suffixes.
- **Wrong:** `let folder_clone = folder.clone(); let root_clone = root.clone();`
- **Right (shadowing):** `let folder = folder.clone();`
- **Right (scoped block):** Wrap the `spawn_blocking` call in a block so the originals
  remain available after:
  ```rust
  {
      let source = source_branch.clone();
      tokio::task::spawn_blocking(move || do_work(&source)).await??;
  }
  // source_branch is still usable here
  ```

### Tests

- Use `#[tokio::test]` for async test functions, not `#[test]`.
- All `sqlx` operations are async and require `.await`.
- For sleep/delays in tests, use `tokio::time::sleep` instead of `std::thread::sleep`.
- Prefer tests to follow the same order as the functions they cover when practical.

### Anti-Patterns to Avoid

- **No sync wrappers:** Do not wrap async code in `Runtime::new()` + `block_on()`. The
  codebase is fully async — keep it that way.
- **No `features = ["full"]`:** Always specify individual tokio features.
- **No `tokio::sync::Mutex` for sync-only guards:** Only use it when the critical
  section contains `.await`.

### UI Render Hot Paths

- In per-frame UI paths (`App::draw()`, `ui::render()`, and page/component `render()`
  helpers), avoid cloning large `String`, `Vec`, or `HashMap` values just to satisfy
  borrow scopes. Prefer borrow splitting, small frame snapshots, or other designs that
  keep render inputs shared.
- When adding cached derived UI data (markdown lines, diff layout, lookup maps, wrapped
  text), keep the cache bounded and document the cache key plus invalidation trigger in
  code comments or docstrings.
- If a layout/count helper and the final paint path need the same expensive derived
  data, route both through the same cache or shared snapshot instead of recomputing the
  full render twice per frame.

## Quality Gates

Use the repository hook catalog in `.pre-commit-config.yaml` as the executable source of
truth for validation commands. Keep agent workflows and CI invoking hook IDs from that
file instead of re-encoding cargo or Zola commands elsewhere. When hooks are added or
renamed there, follow the catalog, not this guide.

### Validation Ladder

Run exactly one rung per situation; do not stack a full sweep on top of targeted checks
that already cover the impact.

1. **While iterating (per edit):** Run the relevant fixer or check on touched files
   only, such as `rustfmt-fix` for Rust sources or `mdformat` for markdown.
1. **Before finalizing a turn, handoff, commit, or review:** Run impact-based validation
   (below) — the narrowest repository-defined checks that cover every touched file,
   expanded through affected workspace dependencies and dependents. Use the dependency
   graph from workspace manifests or `cargo metadata` when deciding which crates and
   tests are affected.
1. **Cross-cutting changes, unclear dependency impact, release work, or low confidence
   in targeted checks:** Run the full suite: `prek run --all-files`, then
   `prek run test-workspace --all-files --hook-stage manual`.

If you cannot confidently prove the targeted checks cover the full impact, escalate to
the full suite.

### Impact-Based Validation

- **Markdown and docs:** Run `mdformat`, then the default hooks for touched paths.
- **`docs/site/` content:** Add `zola-check`; use `djlint-reformat` for HTML templates.
- **Rust sources:** Run `rustfmt-fix` while iterating, then `cargo-check`; add focused
  tests for the changed crate and affected dependents.
- **Workspace crate source tests:** Use the narrowest matching member source-test hook.
- **Cargo manifests and lockfile:** Run `cargo-check`, `clippy`, and affected crate
  tests; use `test-workspace` when dependency impact is broad or uncertain.
- **SQL migrations:** Run `check-migrations` plus Rust checks and tests for crates that
  embed or query those migrations.
- **Hook catalog:** Run `validate-prek-config`.
- **User-visible UI behavior:** Satisfy the MANDATORY feature-test gate, then run the
  focused E2E workflow. Do not run the full E2E feature suite locally;
  `.github/workflows/postsubmit.yml` runs `test-agentty-e2e` on GitHub after merge to
  `main`.

### Autofix Discipline

Run mutating fixers one at a time and inspect the resulting diff after each one:

1. **Format:** `prek run rustfmt-fix --all-files --hook-stage manual`
1. **Inspect:** Review the diff for unexpected formatting churn.
1. **Clippy Fix:** `prek run clippy-fix --all-files --hook-stage manual`
1. **Inspect:** Review the diff for behavior changes before continuing.

### Periodic / CI

Use slower hygiene hooks only in CI or when making broader changes. Consult
`.pre-commit-config.yaml` for current hook IDs and invocations.

- Run local broad checks when relevant: coverage summary, member source tests,
  `zola-check`, `cargo-shear`, and `cargo-audit`.
- Do not run GitHub-only `coverage-lcov` or the full `test-agentty-e2e` suite locally;
  `.github/workflows/postsubmit.yml` owns those after merge to `main`.

### Test Failure Protocol

- **Stuck tests:** If a test produces no output for 5 minutes, kill it and report the
  test name and last output to the user immediately.
- **Failing tests:** Attempt to fix the failing test up to 3 times. After 3 failed
  attempts, stop and report the test name, error output, and what was tried to the user.
- **Do not skip or ignore:** Never mark a failing test as `#[ignore]`, delete it, or
  bypass it to unblock progress.

### Manual Verification

- **Test Style:** Verify every *touched* test function uses explicit `// Arrange`,
  `// Act`, and `// Assert` comments.
  - Combining `Arrange`, `Act`, and `Assert` is allowed when it improves clarity (for
    very small tests).
- **Dependencies:** Verify all dependencies (including dev/build) are defined in the
  root `Cargo.toml` and referenced via `workspace = true`.
- **Boundary Governance:** In `app/` and `runtime/` orchestration code, reject direct
  `Command::new`, `Instant::now`, `SystemTime::now`, and direct filesystem/process calls
  unless they are routed behind an explicit trait boundary.
  - Treat directory walking, `Path::exists`, `Path::is_dir`, `Path::is_file`, `std::fs`,
    `tokio::fs`, and path canonicalization or copy helpers as filesystem boundary calls
    too; keep them in `infra/` and inject traits into orchestration layers.

## Documentation Sync

Apply the smallest documentation update that matches the change. When multiple triggers
apply, update all matching docs before handoff.

- **Added or updated code:** Document the added or updated behavior using docstrings. In
  Rust, add or refresh `///` doc comments for touched public structs, functions, types,
  and closely related sibling or parent elements when needed for clarity.
- **Documentation, commit messages, PR descriptions, or bullets reference code
  elements:** Wrap code elements in backticks.
- **User-facing features, agent backends, models, keybindings, session states, UI pages,
  or visible behavior change:** Update the corresponding page under
  `docs/site/content/docs/`; use the nearest source-side guide for exact routing.
- **Architecture boundaries, runtime flow, trait boundaries, workspace crate ownership,
  modules, or change-path guidance change:** Update the matching architecture docs under
  `docs/site/content/docs/architecture/`; use the nearest source-side guide for exact
  routing.
- **End-user prerequisites, usage instructions, features, or crate information
  changes:** Update `README.md`.
- **Release work changes shipped behavior:** Update `CHANGELOG.md` using Keep a
  Changelog format.

Always wrap these code elements in backticks when referenced in prose:

- Enum variants: `Sessions`, `Roadmap`
- Struct/Type names: `RoadmapPage`, `Tab`, `AppMode`
- Function names: `next_tab()`, `render()`
- Field names: `current_tab`, `table_state`
- Key bindings: `Tab`, `Enter`, `Esc`
- File names: `model.rs`, `AGENTS.md`
- Configuration values: `workspace = true`

### Docs Site Integrity

- Every feature page under `docs/site/content/features/` that declares `[extra].gif`
  must have the matching GIF committed under `docs/site/static/features/`; if GIF
  generation is skipped locally, do not add or keep the feature page yet.
- When keybindings or visible shortcut labels change, verify
  `docs/site/content/docs/usage/keybindings.md` and
  `docs/site/content/docs/usage/workflow.md` against the runtime mode handlers and
  `crates/agentty/src/ui/state/help_action.rs`.
- When forge review-request support changes, keep the docs phrasing aligned with all
  supported forge families and their CLIs, including GitHub/`gh` and GitLab/`glab`.

## Git Conventions

- For all commit preparation and commit message work, use `skills/git-commit/SKILL.md`.
- Never bypass `prek`-managed hooks (see MANDATORY).

## Release Automation

- Release preparation in the repository stops at a normal version-bump commit: update
  package versions locally, run required checks, and land the commit through the usual
  push/merge path.
- Do not create or push release tags from a local checkout. After the version bump
  commit lands, create the release tag for that exact commit in the GitHub UI using the
  `v` prefix (for example, `v0.1.0`).
- GitHub workflows publish the release from the GitHub-created tag. Do not run separate
  local release, tagging, or artifact-publishing steps.
- Treat `.github/workflows/release.yml` as generated output from `dist`. Do not edit
  this workflow file manually.
- To upgrade `cargo-dist`, update `cargo-dist-version` in `dist-workspace.toml`, then
  rerun `dist init` from the repository root so `dist` regenerates
  `.github/workflows/release.yml` and any related release automation changes.
- When updating `cargo-dist`, review and commit the generated changes in
  `dist-workspace.toml` and `.github/workflows/release.yml` together.

## Agent Instructions

- **Pragmatic Abstractions:** Introduce new abstractions only when they provide clear
  payoff (reuse, reduced complexity, or materially better testability). For
  straightforward changes, prefer direct in-place edits with minimal diff.
- **No Pass-Through Wrappers:** Do not introduce functions whose body only forwards to
  another function call. Inline the call instead unless the wrapper adds real behavior,
  a meaningful boundary, or clear naming value that justifies the extra indirection.
- **Readability:** Use descriptive variable names. Do NOT use single-letter variables
  (e.g., `f`, `p`, `c`) or single-letter prefixes. Code should be self-documenting.
- When removing behavior, do not add tests or assertions that only verify the removed
  shortcut, label, or action is absent. Prefer tests that cover the remaining supported
  behavior.
- Structure tests using "Arrange, Act, Assert" comments to clearly separate setup,
  execution, and verification phases.

### AGENTS.md Maintenance

- Update the relevant `AGENTS.md` file only when a user instruction establishes a
  critical, persistent preference, convention, or workflow rule. Do not update it for
  one-off tasks.
- Keep `AGENTS.md` files focused on purpose, entry points, invariants, change routing,
  and docs-sync notes. Do not maintain exhaustive per-directory file inventories.
- In `AGENTS.md`, do not use parent-directory relative paths. Each file should describe
  only its own directory or module boundary.
- When creating a new `AGENTS.md` file in any directory, always create corresponding
  symlinks: `ln -s AGENTS.md CLAUDE.md && ln -s AGENTS.md GEMINI.md` in the same
  directory.

## Skills

- Skills are available under `skills/`, with the summary catalog in `skills/AGENTS.md`.
- Read `skills/AGENTS.md` to discover available skills before selecting one.
- Activate a skill when the user explicitly names it or the task intent matches the
  skill description.
- Use the minimal set of skills needed for the current turn.
- Do not carry a skill across turns unless it is explicitly requested again or clearly
  re-triggered by intent.

## Runtime Prompts

- Agent prompt templates live in `crates/ag-agent/src/agent/template/`,
  `crates/agentty/src/app/template/`, and `crates/ag-protocol/src/template/`. Inspect
  the source templates directly when changing backend prompt behavior.

## Workspace Map

- `crates/` contains all workspace crates.
- `docs/site/content/docs/architecture/` contains the canonical module, runtime, and
  change-path references.
- `skills/` contains reusable workflow skills and their discovery notes.
