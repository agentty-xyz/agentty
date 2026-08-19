# ag-xtask

Deterministic Rust-based workspace maintenance tasks.

## Change Guidance

- Put each task's logic in a focused module and register its CLI dispatch in
  `src/main.rs`.
- Keep commands suitable for local and CI use.
- For workflows with multiple filesystem or process calls, inject a mockable boundary
  and test the orchestration without live side effects.
