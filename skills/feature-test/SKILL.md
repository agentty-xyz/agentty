---
name: feature-test
description: Create or update Agentty PTY feature tests and their GIF, PNG, and Zola artifacts. Use for visible UI behavior demonstrable without live agent backends, or when maintaining the recording workflow.
---

# Feature Tests

Use `FeatureTest` for user-visible UI behavior that can be demonstrated in a PTY without
live agent backends. Backend-only work follows the root integration-test requirements.
For unavailable live infrastructure, report the precise coverage gap and test the
supported boundaries deterministically.

For a new visible feature, deliver a scenario in `crates/agentty/tests/e2e/`, a GIF and
PNG poster in `docs/site/static/features/`, and a page in `docs/site/content/features/`.
The same `snake_case` feature name ties the artifacts together.

## Select the Procedure

Read only the reference needed for the current task:

- [Authoring](references/authoring.md): a canonical `FeatureTest` example, scenario
  helpers, generated page metadata, and deterministic freshness behavior.
- [Recording](references/recording.md): generate or refresh GIFs in the pinned Podman
  container, inspect the result, and create a matching PNG poster.
- [Image maintenance](references/image-maintenance.md): maintainers changing the
  canonical recording image or publishing a replacement digest.

Use the existing scenario and assertion helpers. Assert observable terminal behavior,
keep volatile data deterministic, and let the framework generate VHS tapes.
`FeatureTest::zola()` owns feature title, description, and weight.

## Routine Validation

```sh
# Run the focused feature scenario without launching VHS or Chrome.
TESTTY_GIF_MODE=check cargo nextest run --locked --profile ci -p agentty --test e2e test_{name}

# Validate generated pages and site integration.
prek run zola-check --all-files --hook-stage manual
```

Use `check` for routine validation. An unset `TESTTY_GIF_MODE` runs PTY assertions
without GIF work. Intentional recording uses the canonical container; never launch VHS
or a recording mode directly on the developer host. The recording reference owns the
exact commands and the distinction between the container and the host.

A passing freshness check does not publish an unpublished feature or refresh its PNG.
Before handing off a new or changed published feature, inspect the GIF and poster,
verify both are nonempty, and keep them with the matching hash sidecar and page. Report
unavailable recording or site tooling as a verification gap.

The root `AGENTS.md` owns final quality gates, including the full E2E suite for Rust
changes. Focused iteration does not replace those gates.
