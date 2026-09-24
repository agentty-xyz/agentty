# Authoring Feature Tests

## Naming Convention

For a published feature demo, a single name flows through the entire pipeline:

| Artifact      | Path                                                    |
| ------------- | ------------------------------------------------------- |
| Test function | `test_{name}` in `crates/agentty/tests/e2e/{module}.rs` |
| GIF file      | `docs/site/static/features/{name}.gif`                  |
| PNG poster    | `docs/site/static/features/{name}.png`                  |
| Zola page     | `docs/site/content/features/{name}.md`                  |

Choose a short, descriptive `snake_case` name that describes the feature (e.g.,
`session_creation`, `help_overlay`, `tab_switch`).

### 1. Choose the test module

Place the test in the E2E module that best matches the feature area:

- `session/` — topic modules for session lifecycle, prompts, diffs, reviews, and related
  interactions; `session.rs` registers these modules.
- `navigation.rs` — tab cycling, help overlay, quit dialog.
- `confirmation.rs` — confirmation dialogs.
- `project.rs` — project page and project-related flows.

If no existing module fits, create a new one and register it in
`crates/agentty/tests/e2e/main.rs`, or in `crates/agentty/tests/e2e/session.rs` for a
new session topic.

### 2. Write the test using `FeatureTest`

Use the `FeatureTest` builder from `crates/agentty/tests/e2e/common.rs`. This is the
preferred pattern — it handles `TempDir` and `BuilderEnv` creation, scenario execution,
optional GIF generation with content-hash caching, and optional Zola page creation in a
single declarative chain.

The example below publishes a feature demo. Omit `.zola(...)` when only PTY coverage is
needed.

```rust
use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::FeatureTest;

#[tokio::test]
async fn test_{name}() -> Result<(), Box<dyn std::error::Error>> {
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
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "expected text", &full);
                })
            },
        )
        .await?;

    Ok(())
}
```

#### `FeatureTest` builder methods

- **`new(name)`** — set the feature name (used for GIF filename and Zola page).
- **`.with_git()`** — initialize a git repo in the workdir (required for
  session/worktree features).
- **`.zola(title, description, weight)`** — enable Zola page auto-generation with the
  given frontmatter fields. The page is written only if it does not already exist.
- **`.setup(setup)`** — supply an async fixture closure returning
  `Box::pin(async move { ... })`.
- **`.run(build_scenario, assert).await`** — execute the scenario, await the boxed async
  assertion closure, and generate the GIF when recording mode is enabled.

#### Common `Journey` helpers

Reuse the shared journey builders from `common.rs` instead of repeating step sequences:

- `wait_for_agentty_startup()` — wait for the initial TUI frame.
- `switch_to_tab(name)` — press `Tab` and wait for stability.
- `switch_to_tab_reverse(name)` — press `BackTab` and wait.
- `open_quit_dialog()` — press `q` and wait.
- `open_help_overlay()` — press `?` and wait.
- `create_session_and_return_to_list()` — full session creation flow.
- `create_session_with_prompt_and_return_to_list(prompt)` — session creation with a
  custom prompt.

### 3. Verify the Zola page

If you used `.zola(...)`, `FeatureTest` auto-generates the content page at
`docs/site/content/features/{name}.md` on first run. The generated page uses this
frontmatter:

```toml
+++
title = "Feature title"
description = "One-line description shown on the card."
weight = 50

[extra]
gif = "{name}.gif"
+++
```

The `features.html` template auto-discovers all pages in `content/features/` sorted by
`weight`. No manual template edits are needed.

## Focused Validation

Run the affected scenario without launching VHS or Chrome:

```sh
TESTTY_GIF_MODE=check cargo nextest run --locked --profile ci -p agentty --test e2e test_{name}
```

An unset `TESTTY_GIF_MODE` still runs the PTY assertions without GIF work. Check mode
does not publish a new demo or refresh its PNG poster. When adding or changing a
published page or asset, run `prek run zola-check --all-files --hook-stage manual`. The
root `AGENTS.md` defines the final quality gates.

## Freshness Modes

The `TESTTY_GIF_MODE` environment variable selects the freshness mode used by
`FeatureTest`:

- unset — leave GIF work off while still running the PTY scenario and assertions.
- `generate` / `generate-if-stale` — regenerate when the on-disk hash sidecar is missing
  or stale, otherwise reuse the committed GIF. Use only inside the canonical container,
  which may be launched with Podman on a developer host.
- `check` / `check-only` — compute the would-be hash and compare it to the on-disk
  sidecar without invoking VHS or touching the GIF output directory. The harness fails
  the test when a committed sidecar has drifted, an existing sidecar is invalid, or the
  GIF itself is missing or empty, and surfaces the current/committed hashes plus sidecar
  errors so CI catches drift. Existing GIFs that predate sidecars are tolerated until a
  recording run creates their baseline. `.zola(...)` tests without any committed docs
  page, GIF, or sidecar are treated as unpublished and skipped by check mode until a
  recording run publishes their artifacts.
- `force` / `always` / `always-generate` — bypass the hash cache and re-run VHS
  unconditionally. VHS must be installed: a missing VHS binary fails the test instead of
  being silently skipped, because regeneration was explicitly requested. Use this mode
  only inside the canonical container, never directly in the bare-host environment; the
  recording reference uses `generate` instead.

## Freshness Hash Determinism

The hash only means "the UI moved" if the same UI hashes the same way every run — and on
every machine, because sidecars are committed locally and checked on Linux CI. The
harness already neutralizes the known variance:

- Temp paths are normalized by testty, and `BuilderEnv` keeps every painted directory
  under the test `HOME` so paths render home-collapsed (`~/test-project`,
  `~/.agentty/wt/<hash>`) with a platform-independent length.
- `FeatureTest` pins the wall clock, UTC offset, and rendered version label before
  capture; it also redacts the `wt/<hash>` worktree name a session derives from its
  generated UUID (see `common::session_worktree_redaction`) and the pinned version
  label. Pinning before rendering prevents a wider version from moving styled terminal
  cells and staling every GIF.
- `BuilderEnv` stubs every supported agent CLI, so agent availability — and the default
  agent a new session resolves — does not depend on which real CLIs a machine has.

Anything else volatile a scenario puts on screen — another generated id, a live
duration, a random port — makes every run look stale and re-records the GIF for nothing.
Freeze it in the app under test, or declare it with `FeatureDemo::redact`.

For older scenarios that construct `Scenario` and `FeatureDemo` directly, preserve their
existing assertions while migrating to `FeatureTest` when needed. `FeatureDemo` itself
defaults to `GenerateIfStale`; the Agentty `FeatureTest` wrapper leaves recording off
unless `TESTTY_GIF_MODE` explicitly enables it.
