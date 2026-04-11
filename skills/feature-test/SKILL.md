---
name: feature-test
description: Guide for creating E2E feature tests with VHS GIF generation and Zola feature page auto-discovery for new visible UI features.
---

# Feature Test Skill

Use this skill when adding a new visible UI feature that is user-facing and demonstrable in a PTY scenario. The workflow produces three artifacts from one test:

1. An E2E test in `crates/agentty/tests/e2e/`.
2. A feature GIF in `docs/site/static/features/`.
3. A Zola content page in `docs/site/content/features/`.

## When to Use

A feature test is warranted when **all three** criteria are met:

- **Visible UI behavior change** — the feature renders something new or different on screen.
- **User-facing** — an end user can trigger or observe the behavior through normal interaction.
- **Demonstrable in a scenario** — the behavior can be captured in a short PTY recording without live agent backends.

Skip this skill for internal refactors, backend-only changes, or features that require a running agent to demonstrate.

## Naming Convention

A single name flows through the entire pipeline:

| Artifact | Path |
|----------|------|
| Test function | `test_{name}` in `crates/agentty/tests/e2e/{module}.rs` |
| GIF file | `docs/site/static/features/{name}.gif` |
| Zola page | `docs/site/content/features/{name}.md` |

Choose a short, descriptive `snake_case` name that describes the feature (e.g., `session_creation`, `help_overlay`, `tab_switch`).

## Workflow

### 1. Choose the test module

Place the test in the E2E module that best matches the feature area:

- `session.rs` — session lifecycle and prompt interactions.
- `navigation.rs` — tab cycling, help overlay, quit dialog.
- `confirmation.rs` — confirmation dialogs.
- `project.rs` — project page and project-related flows.

If no existing module fits, create a new one and register it in `crates/agentty/tests/e2e/main.rs`.

### 2. Write the test using `FeatureTest`

Use the `FeatureTest` builder from `crates/agentty/tests/e2e/common.rs`. This is the preferred pattern — it handles `TempDir` and `BuilderEnv` creation, scenario execution, GIF generation with content-hash caching, and optional Zola page creation in a single declarative chain.

```rust
use testty::assertion;
use testty::region::Region;
use testty::scenario::Scenario;

use crate::common;
use crate::common::FeatureTest;

#[test]
fn test_{name}() {
    // Arrange, Act, Assert
    FeatureTest::new("{name}")
        .with_git()   // Required for features that create sessions/worktrees.
        .zola(
            "Human-readable title",
            "One-line description for the feature card.",
            50,  // Weight for ordering on the features page.
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    // Navigate to the relevant tab/state.
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(1500)
                    // Perform the feature interaction.
                    .press_key("a")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("label", "Description of captured state")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "expected text", &full);
            },
        );
}
```

#### `FeatureTest` builder methods

- **`new(name)`** — set the feature name (used for GIF filename and Zola page).
- **`.with_git()`** — initialize a git repo in the workdir (required for session/worktree features).
- **`.zola(title, description, weight)`** — enable Zola page auto-generation with the given frontmatter fields. The page is written only if it does not already exist.
- **`.run(build_scenario, assert)`** — execute the scenario, run assertions, and generate the GIF.

#### Common `Journey` helpers

Reuse the shared journey builders from `common.rs` instead of repeating step sequences:

- `wait_for_agentty_startup()` — wait for the initial TUI frame.
- `switch_to_tab(name)` — press `Tab` and wait for stability.
- `switch_to_tab_reverse(name)` — press `BackTab` and wait.
- `open_quit_dialog()` — press `q` and wait.
- `open_help_overlay()` — press `?` and wait.
- `create_session_and_return_to_list()` — full session creation flow.
- `create_session_with_prompt_and_return_to_list(prompt)` — session creation with a custom prompt.

### 3. Verify the Zola page

If you used `.zola(...)`, `FeatureTest` auto-generates the content page at `docs/site/content/features/{name}.md` on first run. The generated page uses this frontmatter:

```toml
+++
title = "Feature title"
description = "One-line description shown on the card."
weight = 50

[extra]
gif = "{name}.gif"
+++
```

The `features.html` template auto-discovers all pages in `content/features/` sorted by `weight`. No manual template edits are needed.

### 4. Run and verify

```sh
# Run the specific feature test.
cargo test -p agentty --test e2e -- {name}

# Run the full E2E suite to check for regressions.
cargo test -p agentty --test e2e

# Validate the Zola site builds with the new feature page.
zola --root docs/site check
```

The GIF is generated only when VHS is installed. On machines without VHS the test still runs and asserts correctly — GIF generation is gracefully skipped.

Run `zola check` after the test to catch broken frontmatter or template integration before the page reaches CI. This requires Zola to be installed locally — if unavailable, CI catches it via the `pages.yml` workflow.

## Legacy Pattern

Older tests manage `TempDir`, `BuilderEnv`, `Scenario`, and `save_feature_gif` manually instead of using `FeatureTest`. This pattern still works but is not recommended for new tests. Prefer `FeatureTest` for all new feature tests.

```rust
#[test]
fn legacy_example() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");

    let scenario = Scenario::new("{name}")
        .compose(&common::wait_for_agentty_startup())
        // ... build the scenario ...
        .capture_labeled("label", "description");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "expected", &full);

    // GIF generation (manual).
    common::save_feature_gif(&scenario, &report, &env, "{name}");
}
```

When using the legacy pattern, create the Zola page manually at `docs/site/content/features/{name}.md`.

## Checklist

- [ ] Feature name follows `snake_case` naming convention.
- [ ] Test uses `FeatureTest` builder (preferred) or the legacy `Scenario` + `save_feature_gif` pattern.
- [ ] `.with_git()` is set if the feature requires session creation or worktrees.
- [ ] `.zola(...)` is set with a clear title, description, and appropriate weight.
- [ ] Test includes `// Arrange`, `// Act`, and `// Assert` comments (or combined `// Arrange, Act, Assert` for declarative builders).
- [ ] Assertions verify visible UI text or state, not internal implementation details.
- [ ] Test passes with `cargo test -p agentty --test e2e -- {name}`.
- [ ] Full E2E suite passes with `cargo test -p agentty --test e2e`.
- [ ] Zola site validates with `zola --root docs/site check` (when `.zola(...)` is used).
