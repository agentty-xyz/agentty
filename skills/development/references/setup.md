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

The coverage hook needs both Cargo subcommands. CI tooling is configured in
`.github/actions/setup-rust-prek/action.yml`; consult it when reproducing CI setup. Run
`cargo run -p agentty --bin agentty` to launch the application from the workspace.
Public runtime prerequisites and backend authentication are documented in `README.md`.

## Website

Install Zola before running the docs-site gate or previewing `docs/site/`:

```sh
zola serve --root docs/site
```

Use the cataloged `zola-check` hook for validation. Intentional feature recording also
needs a running Podman environment; follow `skills/feature-test/references/recording.md`
for the canonical container workflow.
