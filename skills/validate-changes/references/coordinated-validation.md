# Coordinated Validation

For final Rust validation, the coordinator automatically selects changed packages and
their workspace dependents, then runs `clippy` and unrestricted `test-focused` through
`prek`:

```sh
prek run validate --all-files --hook-stage manual
```

## Selection

Selection covers committed changes since the fork from `origin/main`, staged and
unstaged changes, deleted and renamed paths, and untracked files. Shared or unowned
paths conservatively select the whole workspace. A missing base or empty change set
fails; select packages explicitly for a clean checkout. The command prints the selected
packages, hooks, paths, and reason before running checks.

Preview that plan without running checks or writing reuse stamps:

```sh
cargo run --locked -q -p ag-xtask -- validate --plan
```

Use `--base` with an existing ref to compare against a different branch. Explicit
`--packages` or `AGENTTY_TEST_PACKAGES` replaces automatic path selection; dependents
are still included:

```sh
AGENTTY_TEST_PACKAGES='ag-git ag-agent' \
  prek run validate --all-files --hook-stage manual
```

## Clippy Scope

The coordinator passes the affected selection to its Clippy and `cargo-check`
subprocesses in `AGENTTY_CLIPPY_PACKAGES`, so the hook's full file inventory cannot
widen it. Ordinary `clippy` invocations select owners and dependents from their changed
files, and root configuration changes select the whole workspace. `clippy` also lints
Agentty's explicit PTY targets, with unchanged feature selection, when the dependency
graph (including development dependencies) says Agentty is affected, or when shared
files or an unspecified scope apply. The manual `clippy-agentty-e2e` hook runs just
those targets for diagnosis; default validation already includes them through `clippy`.

## Locks, Target Directories, and Reuse

Concurrent coordinator runs wait on an OS lock in Cargo's effective target directory;
direct Cargo commands use only Cargo's own build lock. Check subprocesses clear `SKIP`
and `PREK_SKIP`, so requested checks always execute. Keep target directories inside the
checkout and free of tracked source files, with downloaded registry and compiler caches
in their default shared locations; target artifacts are excluded from input
fingerprints.

Direct `ag-xtask validate` invocation accepts ordered `--hook` IDs and optional
`--reuse`; fresh execution is the default. Reuse applies only to allowlisted
deterministic hooks whose package selection, tracked and untracked source, Git state and
configuration (only the checked-out branch's `branch.<name>.*` section), tool versions,
and environment all match. Changes or failures invalidate success, and inputs changing
during execution cannot produce a successful stamp. Coverage always runs fresh.
Repository-required hooks still apply.
