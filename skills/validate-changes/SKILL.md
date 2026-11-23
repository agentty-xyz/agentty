---
name: validate-changes
description: Choose and run the prek checks that cover a change, from formatters and focused tests during iteration to the final validation rung before handoff, and diagnose failing or long-running checks.
---

# Validate Changes

Use this skill for every change: while iterating, before handoff, and when a CI hook
fails. Root `AGENTS.md` decides which gates are required; this skill says how to run
them. Invoke each check's definition through `prek`; do not copy hook implementations
into ad hoc commands.

## Iterate

1. Run the relevant formatter or fixer on touched paths and inspect its diff. Run
   mutating fixers one at a time. A fixer that rewrites files reports `Failed`; that is
   its result, not an error, so review the diff instead of rerunning it. `rustfmt-fix`
   needs no `--files` list: it formats Rust files changed since the fork from
   `origin/main`, or the whole workspace when formatter configuration or the toolchain
   changes.

1. Catch compile errors with
   `prek run cargo-check --hook-stage manual --files <changed paths>`. It compile-checks
   the packages `clippy` would select for those paths, without linting.

1. Run focused tests for the changed behavior with `test-focused` and an explicit
   `AGENTTY_TEST_FILTER`:

   ```sh
   AGENTTY_TEST_PACKAGES='ag-git' \
     AGENTTY_TEST_FILTER='package(=ag-git) and test(worktree)' \
     prek run test-focused --all-files --hook-stage manual
   ```

   For a focused PTY scenario, use the command in `skills/write-feature-tests/SKILL.md`,
   not the full E2E hook.

## Choose the Final Rung

Before handoff (the task's final state, not each reply in a multi-turn session), choose
one validation rung covering every touched file and all affected dependents:

| Impact                              | Required final checks                                                                                    |
| ----------------------------------- | -------------------------------------------------------------------------------------------------------- |
| Markdown                            | `mdformat` and default hooks for touched paths                                                           |
| Docs site                           | Markdown checks plus `zola-check`; format touched templates with `djlint-reformat`                       |
| Rust                                | `rustfmt-fix`, `clippy`, `ast-grep`, and unrestricted `test-focused` for changed packages and dependents |
| Rustdoc or crate README examples    | Add `test-doc`, since nextest runs no doctests                                                           |
| Manifests, migrations, hook catalog | Add their dedicated checks from `.pre-commit-config.yaml`                                                |
| `ag-harness` or `ag-router` API     | Add `public-api`                                                                                         |
| ast-grep rules (`lint/`)            | `ast-grep`, `ast-grep-test`, and `check-standards`                                                       |

Final Rust selection must retain public-contract integration tests; source-only hooks do
not replace them. Remove test-name restrictions for affected packages, or use
`test-workspace`. For cross-cutting changes, run `prek run --all-files` plus affected
package tests after touched paths pass formatting and focused tests.

The `validate` coordinator selects changed packages and their dependents and runs
`clippy` and unrestricted `test-focused` for them:

```sh
prek run validate --all-files --hook-stage manual
```

Read `skills/validate-changes/references/coordinated-validation.md` before overriding
its selection, comparing against another base, or reusing earlier results.

## Select Affected-Package Tests by Hand

For final affected-package validation without the coordinator, drop the test-name
restriction and select the changed package with its dependents: `rdeps(=ag-git)` selects
`ag-git` and every package that depends on it. Dependencies are compiled but not
retested, because their sources did not change. Combine selections with `or` when
several packages change:

```sh
AGENTTY_TEST_FILTER='rdeps(=ag-git)' \
  prek run test-focused --all-files --hook-stage manual
```

`AGENTTY_TEST_PACKAGES` optionally limits compilation to a whitespace-separated list of
Cargo package names; without it, the hook builds the workspace. Cargo still builds
dependencies the selected packages require. An execution filter does not select
compilation packages, so for final validation either list every changed package and
dependent, or omit the build limit and use the `rdeps` filter.

`test-focused` requires a nonempty filter and fails when no tests match. It and
`test-workspace` retain public integration tests. Only the Agentty PTY suites are left
out: their `test = false` manifest entries reserve them for the separate
`test-agentty-e2e` gate, so other runs do not compile them.

`test-focused` uses the `local` nextest profile, which inherits CI's resource groups,
timeouts, and retry policy but stops scheduling tests after the first failure and waits
for running tests to finish. CI hooks keep the exhaustive `ci` profile, so one failure
does not prevent collection of others. A failing local run never satisfies final
validation or creates a reusable success stamp.

## Diagnose Failures and Long Checks

- Let the runner's configured timeouts govern individual tests. Run long checks in the
  foreground, redirect large output to a log, and read its summary after the command
  exits; do not poll with repeated `tail`, `ps`, or sleep loops. After five minutes
  without output, inspect build and process activity once; terminate and report a
  confirmed stall, not silence alone.
- Diagnose a CI failure with its failing hook alone, adding `--hook-stage manual` where
  required, rather than rerunning every gate after each edit.
- After three failed repair attempts, stop and report the test, output, and attempted
  fixes; never skip, ignore, or delete the test.

## Special Cases

- **TUI snapshots:** use the `test-agentty-e2e` hook for the final suite. Set
  `TUI_TEST_UPDATE=1` on the `prek run` invocation only when intentionally refreshing
  snapshot baselines. Follow `skills/write-feature-tests/SKILL.md` for focused scenarios
  and `skills/refresh-feature-recordings/SKILL.md` for published recordings.
- **Coverage gaps, `ag-harness` feature builds, and SonarQube issues:** follow
  `skills/validate-changes/references/coverage.md`.
- **Public API snapshots:** `public-api` compares `crates/ag-harness/public-api.txt` and
  `crates/ag-router/public-api.txt` with the current APIs. When the diff is an intended,
  reviewed change, rewrite both with
  `AGENTTY_PUBLIC_API_UPDATE=1 prek run public-api --hook-stage manual --all-files` and
  commit them with the change.
- **Test and build timings, or a `check-slow-tests` failure:** follow
  `skills/validate-changes/references/test-timings.md`.
- **Changes to `check-instructions` or to the test and coverage hooks:** follow
  `skills/validate-changes/references/hook-maintenance.md`.

## Handoff

Record each command, its scope, and its result. Name the rung chosen and any check that
was blocked, failed, or reused from an earlier unchanged state; never report a check
that did not run as passing.
