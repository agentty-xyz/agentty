---
name: feature-test
description: Guide for creating E2E feature tests with VHS GIF generation and Zola feature page auto-discovery for new visible UI features.
---

# Feature Test Skill

Use this skill when adding a new visible UI feature that is user-facing and demonstrable
in a PTY scenario. The workflow produces four artifacts from one test:

1. An E2E test in `crates/agentty/tests/e2e/`.
1. A feature GIF in `docs/site/static/features/`.
1. A static PNG poster in `docs/site/static/features/`.
1. A Zola content page in `docs/site/content/features/`.

## When to Use

A feature test is warranted when **all three** criteria are met:

- **Visible UI behavior change** — the feature renders something new or different on
  screen.
- **User-facing** — an end user can trigger or observe the behavior through normal
  interaction.
- **Demonstrable in a scenario** — the behavior can be captured in a short PTY recording
  without live agent backends.

Skip this skill for internal refactors, backend-only changes, or features that require a
running agent to demonstrate.

## Naming Convention

A single name flows through the entire pipeline:

| Artifact      | Path                                                    |
| ------------- | ------------------------------------------------------- |
| Test function | `test_{name}` in `crates/agentty/tests/e2e/{module}.rs` |
| GIF file      | `docs/site/static/features/{name}.gif`                  |
| PNG poster    | `docs/site/static/features/{name}.png`                  |
| Zola page     | `docs/site/content/features/{name}.md`                  |

Choose a short, descriptive `snake_case` name that describes the feature (e.g.,
`session_creation`, `help_overlay`, `tab_switch`).

## Workflow

### 1. Choose the test module

Place the test in the E2E module that best matches the feature area:

- `session.rs` — session lifecycle and prompt interactions.
- `navigation.rs` — tab cycling, help overlay, quit dialog.
- `confirmation.rs` — confirmation dialogs.
- `project.rs` — project page and project-related flows.

If no existing module fits, create a new one and register it in
`crates/agentty/tests/e2e/main.rs`.

### 2. Write the test using `FeatureTest`

Use the `FeatureTest` builder from `crates/agentty/tests/e2e/common.rs`. This is the
preferred pattern — it handles `TempDir` and `BuilderEnv` creation, scenario execution,
GIF generation with content-hash caching, and optional Zola page creation in a single
declarative chain.

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
- **`.with_git()`** — initialize a git repo in the workdir (required for
  session/worktree features).
- **`.zola(title, description, weight)`** — enable Zola page auto-generation with the
  given frontmatter fields. The page is written only if it does not already exist.
- **`.run(build_scenario, assert)`** — execute the scenario, run assertions, and
  generate the GIF.

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

### 4. Create or refresh the PNG poster

Every feature GIF needs a same-named PNG poster for the site's `prefers-reduced-motion`
and `noscript` fallbacks. Create the poster when adding a GIF, and regenerate it
whenever the GIF changes. `TESTTY_GIF_MODE=check` verifies GIF freshness but does not
verify or update the poster.

First inspect the finished GIF and choose a timestamp that clearly communicates the
feature's result. Prefer a stable end state with the important UI visible. Do not
automatically use the first or midpoint frame: it may show an empty, loading, or
transitional state.

Check the GIF duration before choosing a timestamp:

```sh
ffprobe -v error \
  -show_entries format=duration \
  -of default=noprint_wrappers=1 \
  docs/site/static/features/<name>.gif
```

Extract exactly one frame with `ffmpeg`, preserving the aspect ratio and capping the
width at 1600 pixels without upscaling:

```sh
ffmpeg -y \
  -i docs/site/static/features/<name>.gif \
  -ss <timestamp> \
  -vf "scale=w='min(1600\,iw)':h=-1" \
  -frames:v 1 \
  -compression_level 9 \
  docs/site/static/features/<name>.png
```

Use a timestamp accepted by `ffmpeg`, such as `00:00:01.500`, that falls within the
reported duration. Open the resulting PNG and verify that it is sharp, legible, free of
transitional artifacts, and still represents the current GIF. If it does not, choose a
better timestamp and rerun the command.

Check that every GIF declared by a feature page has a nonempty poster before finalizing:

```sh
for feature_page in docs/site/content/features/*.md; do
  gif_name=$(sed -n 's/^gif = "\(.*\)"/\1/p' "${feature_page}")
  test -z "${gif_name}" && continue
  poster_path="docs/site/static/features/${gif_name%.gif}.png"
  test -s "${poster_path}" || {
    echo "missing PNG poster: ${poster_path}"
    exit 1
  }
done
```

### 5. Run and verify

```sh
# Run the focused E2E test for the feature.
TESTTY_GIF_MODE=check cargo nextest run --locked --profile ci -p agentty --test e2e test_{name}

# Validate the Zola site builds with the new feature page.
prek run zola-check --all-files --hook-stage manual
```

Routine agent validation must use `TESTTY_GIF_MODE=check` so the semantic PTY assertions
still run while GIF freshness is checked without invoking VHS or launching Chrome. Use
`TESTTY_GIF_MODE=force` only when intentionally regenerating GIF assets.

### Record in the canonical container

Committed hash sidecars must be produced by the same environment that CI verifies them
in. `docker/e2e.Dockerfile` defines that environment: a pinned `linux/amd64` image with
the Rust toolchain, `prek`, `cargo-nextest`, and the full VHS recording stack (`vhs`,
`ttyd`, Chromium, `ffmpeg`, JetBrains Mono). Presubmit and postsubmit build this image
and run the `test-agentty-e2e` hook inside it against a read-only checkout, so record
GIFs in the same image instead of on the host. The host needs Docker only — no local
Chrome or VHS — and the localhost-socket sandbox restriction below does not apply inside
the container.

Build the image (rebuild only when the Dockerfile changes):

```sh
docker build --platform linux/amd64 \
  --file docker/e2e.Dockerfile --tag agentty-e2e docker
```

Record or refresh feature artifacts with a writable workspace mount and `generate` mode,
which re-records only missing or stale GIFs. The two named volumes cache the cargo
registry and build output between runs:

```sh
docker run --rm --platform linux/amd64 \
  --mount type=bind,source="$PWD",target=/workspace \
  --mount type=volume,source=agentty-e2e-cargo,target=/usr/local/cargo/registry \
  --mount type=volume,source=agentty-e2e-target,target=/tmp/target \
  --env CARGO_TARGET_DIR=/tmp/target \
  --env TESTTY_GIF_MODE=generate \
  agentty-e2e \
  cargo nextest run --locked --profile ci -p agentty --test e2e test_{name}
```

Review the changed GIF and `.{name}.hash` sidecar, then refresh the PNG poster for every
regenerated GIF (section 4) before committing all three together. Successful generation
removes the previous same-named PNG intentionally so a stale poster cannot pass the
nonempty-poster integrity check; a failed recording preserves the last valid poster.

### Host recording caveats

`force` mode must run from an unsandboxed shell — a normal user terminal, or a single
agent command explicitly approved to bypass the sandbox. VHS records through localhost
sockets (`ttyd` plus the Chrome DevTools protocol), and sandboxed agent shells deny all
network, so `vhs` segfaults in `randomPort()` before Chrome ever launches. That failure
is easy to misdiagnose as a missing browser binary: having Chrome, `vhs`, `ttyd`, and
`ffmpeg` installed is not enough — the shell must also allow localhost sockets.

In default `generate-if-stale` mode, machines without VHS still run and assert the test
correctly, then gracefully skip GIF generation. In `force` mode, VHS must be installed
because regeneration was explicitly requested.

The `TESTTY_GIF_MODE` env var selects the freshness mode used by `FeatureTest`:

- unset / `generate` / `generate-if-stale` (default) — regenerate when the on-disk hash
  sidecar is missing or stale, otherwise reuse the committed GIF.
- `check` / `check-only` — compute the would-be hash and compare it to the on-disk
  sidecar without invoking VHS or touching the GIF output directory. The harness fails
  the test when a committed sidecar has drifted, an existing sidecar is invalid, or the
  GIF itself is missing, and surfaces the current/committed hashes plus sidecar errors
  so CI catches drift. Existing GIFs that predate sidecars are tolerated until a
  recording run creates their baseline. `.zola(...)` tests without any committed docs
  page, GIF, or sidecar are treated as unpublished and skipped by check mode until a
  recording run publishes their artifacts.
- `force` / `always` / `always-generate` — bypass the hash cache and re-run VHS
  unconditionally. VHS must be installed: a missing VHS binary fails the test instead of
  being silently skipped, because regeneration was explicitly requested.

Run `prek run zola-check --all-files --hook-stage manual` after the test to catch broken
frontmatter or template integration before the page reaches CI. This requires Zola to be
installed locally — if unavailable, CI catches it via the `pages.yml` workflow.

### Freshness hash determinism

The hash only means "the UI moved" if the same UI hashes the same way every run — and on
every machine, because sidecars are committed locally and checked on Linux CI. The
harness already neutralizes the known variance:

- Temp paths are normalized by testty, and `BuilderEnv` keeps every painted directory
  under the test `HOME` so paths render home-collapsed (`~/test-project`,
  `~/.agentty/wt/<hash>`) with a platform-independent length.
- `FeatureTest` pins the wall clock, redacts the `wt/<hash>` worktree name a session
  derives from its generated UUID (see `common::session_worktree_redaction`), and
  redacts the `Agentty v<version>` header so release bumps do not stale every GIF.
- `BuilderEnv` stubs every supported agent CLI, so agent availability — and the default
  agent a new session resolves — does not depend on which real CLIs a machine has.

Anything else volatile a scenario puts on screen — another generated id, a live
duration, a random port — makes every run look stale and re-records the GIF for nothing.
Freeze it in the app under test, or declare it with `FeatureDemo::redact`.

## Legacy Pattern

Older tests manage `TempDir`, `BuilderEnv`, `Scenario`, and `save_feature_gif` manually
instead of using `FeatureTest`. This pattern still works but is not recommended for new
tests. Prefer `FeatureTest` for all new feature tests.

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

When using the legacy pattern, create the Zola page manually at
`docs/site/content/features/{name}.md`.

## Checklist

- [ ] Feature name follows `snake_case` naming convention.
- [ ] Test uses `FeatureTest` builder (preferred) or the legacy `Scenario` +
  `save_feature_gif` pattern.
- [ ] `.with_git()` is set if the feature requires session creation or worktrees.
- [ ] `.zola(...)` is set with a clear title, description, and appropriate weight.
- [ ] Test includes `// Arrange`, `// Act`, and `// Assert` comments (or combined
  `// Arrange, Act, Assert` for declarative builders).
- [ ] Assertions verify visible UI text or state, not internal implementation details.
- [ ] A same-named PNG poster exists, shows a meaningful stable frame, and was refreshed
  after the latest GIF change.
- [ ] Focused E2E workflow passes with
  `TESTTY_GIF_MODE=check cargo nextest run --locked --profile ci -p agentty --test e2e test_{name}`.
- [ ] Zola site validates with `prek run zola-check --all-files --hook-stage manual`
  (when `.zola(...)` is used).
