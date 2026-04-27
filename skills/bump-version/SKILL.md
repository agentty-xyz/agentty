---
name: bump-version
description: Guide for bumping project versions and running base release-preparation validations.
---

# Bump Version Workflow

Use this skill when preparing a version bump for the project. The repository's
current release flow is defined in `AGENTS.md`; this skill only covers choosing
the bump, updating version files, and running the baseline validations that make
the version-bump change ready for normal review. Do not duplicate commit, tag,
push, or pull workflow steps here.

## Workflow

1. **Version Selection**

   - Always ask the user which version bump to apply: `major`, `minor`, or `patch`.
   - Do not update versions until the user confirms one of these options.
   - Use the current repository release pattern when advising:
     - Prefer `patch` by default for fixes, small UX improvements, refactors, docs
       updates, model/config changes, and other incremental work within the current
       `0.y` line.
     - Use `minor` for milestone cuts: new top-level workflows, pages, modes, or
       cross-cutting runtime/protocol changes that feel like a new product phase.
     - Treat this as a pragmatic pre-`1.0` policy. The changelog history shows patch
       releases may still include additive features or removals while the project
       remains below `1.0.0`.
   - If the user wants a conservative release recommendation and the change is not
     clearly milestone-sized, recommend `patch`.

1. **Version Bump**

   - Update the `version` field in the root `Cargo.toml`.
   - Update any package lockfile or generated metadata that changes as a direct result
     of the package version update.

1. **Changelog**

   - Update `CHANGELOG.md`.
   - Resolve the current date in Pacific time before writing the changelog date
     (`TZ=America/Los_Angeles date +%F`), so late-day release work follows the
     repository's PST/PDT release calendar.
   - Ensure there is an entry for the new version with the Pacific date:
     `## [vX.Y.Z] - YYYY-MM-DD`.
   - Ensure content adheres to [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
   - Add a `### Contributors` section under the new release entry with a bullet list of
     GitHub usernames (example: `- @minev-dev`).
   - Build the contributor list from commits since the previous tag and deduplicate
     names.

1. **Base Validation**

   - Run the baseline repository validations from the `prek` hook catalog in
     `.pre-commit-config.yaml`:
     - `prek run --all-files`
     - `prek run clippy --all-files --hook-stage manual`
     - `prek run test-workspace --all-files --hook-stage manual`
   - If validation fails, fix the underlying issue and rerun the failing hook before
     handoff.
