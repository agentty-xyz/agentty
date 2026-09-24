# Development Setup

Use the repository-pinned toolchain in `rust-toolchain.toml`. Install Rust through
Rustup, then prepare the formatter, linter, and coverage components:

```sh
rustup toolchain install --profile minimal --no-self-update
rustup component add clippy rustfmt llvm-tools-preview
```

Install `uv` using its
[official installation guide](https://docs.astral.sh/uv/getting-started/installation/).
Install the check runner and Rust test tools:

```sh
uv tool install prek
prek install -f
cargo install cargo-llvm-cov
cargo install cargo-nextest --locked
```

The coverage hook needs both Cargo subcommands. It also runs the native `ag-harness`
sandbox qualification, which fails closed without enforcement: Linux hosts need
Bubblewrap at `/usr/bin/bwrap` with unprivileged user namespaces, mirrored for CI in
`.github/actions/setup-native-sandbox/action.yml`. CI tooling is configured in
`.github/actions/setup-rust-prek/action.yml`; consult it when reproducing CI setup. Run
`cargo run -p agentty --bin agentty` to launch the application from the workspace.
Public runtime prerequisites and backend authentication are documented in `README.md`.

## Build Cache and Parallelism

Install `sccache` to share third-party dependency compilations across worktrees:

```sh
cargo install sccache --locked
```

The Cargo wrapper uses `sccache` when its server is available and otherwise invokes the
compiler directly. Worktrees retain separate `target/` directories. `SCCACHE_DIR` can
select the cache location. Registry dependencies compile from the same paths in every
worktree, so a fresh worktree reuses their cached results. Workspace crates do not:
`sccache` 0.14.0 keys Rust compilations on absolute source paths, and `SCCACHE_BASEDIRS`
does not normalize them, so identical workspace sources in two checkouts miss. Keep
Cargo's default incremental compilation for workspace crates; disabling it makes them
cacheable only for rebuilds within the same checkout. Before documenting cross-worktree
reuse of workspace crates after an upstream change, confirm a cache hit for identical
sources built from two checkout roots.

Cargo uses its CPU-based job default. When running several sessions, set
`CARGO_BUILD_JOBS=2` (or another suitable budget) in each session's environment. To opt
out of caching, set `RUSTC_WRAPPER=`.

Coverage hooks explicitly use `target/llvm-cov-target`, separate from ordinary builds,
even when `CARGO_TARGET_DIR` is set. Override `CARGO_LLVM_COV_TARGET_DIR` to relocate
coverage artifacts; keep it separate from the ordinary build directory. This makes
`cargo-llvm-cov`'s existing default isolation explicit rather than introducing a new
cache optimization.

## Website

Install Zola before running the docs-site gate or previewing `docs/site/`:

```sh
zola serve --root docs/site
```

Use the cataloged `zola-check` hook for validation. Intentional feature recording also
needs a running Podman environment; follow `skills/feature-test/references/recording.md`
for the canonical container workflow.
