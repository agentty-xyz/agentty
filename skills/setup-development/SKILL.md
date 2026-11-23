---
name: setup-development
description: Prepare an Agentty checkout for development, install the toolchain and check runner, configure the shared build cache and parallelism, and reproduce CI tooling.
---

# Set Up Development

Use this skill when preparing a fresh checkout or worktree, when a hook fails because a
tool is missing, or when reproducing CI tooling. `README.md` documents public runtime
prerequisites and backend authentication; this skill covers contributor tooling only.

## Install the Toolchain

1. Install Rust through Rustup with the toolchain pinned in `rust-toolchain.toml`, plus
   the formatter, linter, and coverage components:

   ```sh
   rustup toolchain install --profile minimal --no-self-update
   rustup component add clippy rustfmt llvm-tools-preview
   ```

1. Install `uv` using its
   [official installation guide](https://docs.astral.sh/uv/getting-started/installation/),
   then the check runner and Rust test tools:

   ```sh
   uv tool install prek
   prek install -f
   cargo install cargo-llvm-cov
   cargo install cargo-nextest --locked
   ```

   Use `cargo-nextest` 0.9.115 or newer, enforced by `.config/nextest.toml`, so the
   local profile inherits CI's resource limits and timeouts. `prek install -f` installs
   the fast `pre-commit` checks; CI runs the full validation gates.

1. Launch the application from the workspace with `cargo run -p agentty --bin agentty`.

## Tooling for Specific Hooks

- **Coverage:** the coverage hook needs both Cargo subcommands. It also runs the native
  `ag-harness` sandbox qualification, which fails closed without enforcement: Linux
  hosts need Bubblewrap at `/usr/bin/bwrap` with unprivileged user namespaces, as CI
  provisions in `.github/actions/setup-native-sandbox/action.yml`. Consult
  `.github/actions/setup-rust-prek/action.yml` when reproducing CI tooling.

- **Published-crate checks:** the `cargo-semver-checks` hook needs `cargo-semver-checks`
  at the version `.github/actions/setup-rust-prek/action.yml` pins, for example
  `cargo install cargo-semver-checks --version <pinned version> --locked`; a newer
  nightly can require a newer release to read its rustdoc JSON.

- **Public API snapshots:** the `public-api` hook needs `cargo-public-api` at the
  version `.github/actions/setup-rust-prek/action.yml` pins, installed the same way.

- **`ag-release` tests:** its npm package tests install and run the package with Node.js
  18 or newer and `npm`, which GitHub-hosted runners provide.

- **`ag-remote` tests:** the web client tests run its JavaScript with Node.js 22 or
  newer, which GitHub-hosted runners provide.

- **Timing collector:** changes under `.github/actions/test-timing/` or to the hook
  catalog also run `test-timing-action`, which requires Node.js 24. GitHub-hosted
  runners provide it, and the JavaScript action uses the runner's Node.js runtime inside
  E2E jobs. This is a repository tooling requirement, not an application runtime
  dependency.

- **Website:** install Zola before running the `zola-check` hook or previewing
  `docs/site/`:

  ```sh
  zola --root docs/site serve
  ```

  The local preview uses the production PostHog project from `docs/site/config.toml`, so
  local visits send real events; exclude them in PostHog by filtering on `$host`.
  Session replay starts only after allowing analytics and requires Session Replay to be
  enabled in the PostHog project's settings.

- **Design gallery:** changes to `design/`, the docs site's styles, templates, or
  scripts, or the remote client's assets run the `design-gallery` hook, which needs
  Zola, Node.js, and a Chromium-based browser: Google Chrome or Chromium in its usual
  install path, a headless shell that Playwright installed, or any of them named by
  `CHROME_PATH`. Changes to the gallery check itself run `design-gallery-check-test`,
  which needs Node.js 22.8 or newer for its coverage flags. GitHub-hosted runners
  provide Node.js and Chrome, and the workflow installs Zola.

- **Feature recording:** intentional recording also needs a running Podman environment;
  follow `skills/refresh-feature-recordings/references/recording.md` for the canonical
  container workflow.

## Build Cache and Parallelism

Install `sccache` to share third-party dependency compilations across worktrees:

```sh
cargo install sccache --locked
```

The Cargo wrapper uses `sccache` when its server is available and otherwise invokes the
compiler directly. It also retries directly when `sccache` returns status `254`, such as
when its server retains a deleted temporary directory, and preserves other exit
statuses. Keep the wrapper enabled for routine and agent sessions; set `RUSTC_WRAPPER=`
only to diagnose the cache itself. `SCCACHE_DIR` can select the cache location.

Worktrees keep separate `target/` directories. Registry dependencies compile from the
same paths in every worktree, so a fresh worktree reuses their cached results. Workspace
crates do not: `sccache` 0.14.0 keys Rust compilations on absolute source paths, and
`SCCACHE_BASEDIRS` does not normalize them. Keep Cargo's default incremental compilation
for workspace crates; disabling it makes them cacheable only for rebuilds within the
same checkout. Before documenting cross-worktree reuse of workspace crates after an
upstream change, confirm a cache hit for identical sources built from two checkout
roots.

Cargo uses its CPU-based job default. When running several sessions, set
`CARGO_BUILD_JOBS=2` (or another suitable budget) in each session's environment.

Coverage hooks use `target/llvm-cov-target`, separate from ordinary builds, even when
`CARGO_TARGET_DIR` is set; this makes `cargo-llvm-cov`'s default isolation explicit.
Override `CARGO_LLVM_COV_TARGET_DIR` to relocate coverage artifacts, keeping it separate
from the ordinary build directory. Each coverage hook deletes its raw `.profraw`
profiles when it exits; the report it wrote is what later steps read.

## Handoff

Report the tools installed or found missing, their versions where a minimum applies, and
any hook still blocked by an unavailable tool or platform requirement.
