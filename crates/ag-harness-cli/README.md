# `ag-harness-cli`

Interactive command-line chat powered by the `ag-harness` model runtime.

Set the provider credentials, then start a chat:

```sh
MODEL_API_KEY=your-key cargo run -p ag-harness-cli -- run muse-spark-1.3
```

The package keeps the executable name `ag-harness`. The current directory is readable by
the model by default. Use `--read-dir <DIR>` to choose another root; files read beneath
it are sent to the configured provider. Use `--provider <muse|kimi|qwen>` to select a
provider. Muse uses `MODEL_API_KEY` and the optional `MODEL_API_BASE_URL`; Kimi uses
`KIMI_API_KEY` and `KIMI_BASE_URL`; Qwen uses `DASHSCOPE_API_KEY` and
`DASHSCOPE_BASE_URL`. `--base-url` overrides the corresponding URL variable.

- **Writes** - disabled by default; use `--allow-write` to permit patches beneath the
  selected root.
- **Prompts** - pass an optional prompt after the model for the first turn; further
  prompts are read from standard input.

Provider names, known model identifiers, credential variables, and endpoint defaults
come from `ag-harness`. The CLI owns argument parsing, application prompts, terminal
interaction, and output formatting, so extending the library catalog does not require a
matching CLI provider branch.

Run `cargo run -p ag-harness-cli -- --help` for commands.
