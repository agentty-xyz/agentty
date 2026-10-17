---
name: refresh-feature-recordings
description: Refresh Agentty's published feature GIFs, hash sidecars, and PNG posters when captured UI, fixtures, or recording settings change, or diagnose recording-freshness failures.
---

# Refresh Feature Recordings

Use the existing procedures in `docs/contributing/feature-test/recording.md` for
container recording and poster extraction, and
`docs/contributing/feature-test/authoring.md` for focused freshness checks. Keep image
digests, container commands, and rendering parameters in those canonical references.
Root `AGENTS.md` defines the required validation gates. During a read-only review,
identify missing or stale artifacts without regenerating them.

## Identify the Affected Recordings

- Trace the changed UI through published `FeatureTest` scenarios, including shared
  headers, footers, layouts, themes, and fixture output. A scenario can need refreshed
  artifacts even when its test source is unchanged. Rendering-preset changes can affect
  every published recording.
- Map each affected scenario's actual Rust test function to its `FeatureTest::new` name
  and `.zola(...)` metadata. Function names do not always match artifact names. The
  bundle lives under `docs/site/static/features/`: `{name}.gif`, `.{name}.hash`, and
  `{name}.png`, with a page under `docs/site/content/features/`.
- Run the documented focused checks with `TESTTY_GIF_MODE=check` to establish the
  affected scope. A stale hash can fail before behavioral assertions run. Distinguish
  intended visual changes from failed assertions or nondeterministic captures before
  accepting a new baseline. Investigate varying hashes for unchanged UI; do not
  repeatedly regenerate baselines to conceal a race.

## Refresh the Complete Bundle

1. Check that a permitted recording environment is running before starting generation.
   Use the documented pinned container and native architecture, writable mounts, and
   permitted cache location. Reuse an available environment without asking for approval
   already supplied by the task. If the environment is unavailable, report the exact
   blocker and unfinished artifacts; do not disable freshness checks or substitute
   bare-host VHS recording.
1. Generate all affected recordings through the canonical procedure, selecting their
   actual test functions. Let the recorder produce GIFs and hash sidecars together;
   never edit a sidecar to accept a stale GIF. Verify generation succeeded and each GIF
   and sidecar is nonempty. Keep feature-page metadata owned by `.zola(...)` in sync
   when that metadata changes.
1. Refresh the same-named PNG for every regenerated GIF. Successful generation removes
   the old poster intentionally. Inspect the GIF, choose a representative stable frame,
   extract it with the documented poster procedure, and open the resulting PNG to check
   legibility and relevance. Do not automatically choose the first or midpoint frame. A
   passing hash check does not validate the poster.

## Verify and Hand Off

- After the final rendering and artifact changes, run focused freshness checks for all
  affected scenarios and the documented nonempty-poster check. Run `zola-check` and the
  applicable default hooks. Use the full `test-agentty-e2e` hook when diagnosing its CI
  failure or otherwise required by the root validation policy.
- Review the diff for complete GIF, sidecar, and poster bundles, missing artifacts, and
  unintended changes. Preserve unrelated work and remove session-created temporary files
  according to the task's workspace constraints.
- Report the refreshed scenarios and the checks actually observed. Separate successful
  generation, freshness verification, and behavioral coverage. Explicitly report any
  remaining failures or blocked recording work; partial success is not a green suite.
