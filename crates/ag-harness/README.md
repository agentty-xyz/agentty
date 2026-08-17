# `ag-harness`

Run a model with repository-scoped tools and validated structured output.

```rust
use ag_harness::{Harness, MUSE_SPARK_1_2, Muse, Tool};

let harness = Harness::new(Muse::from_env(MUSE_SPARK_1_2)?)
    .repository(repository_root)
    .allow(Tool::Read);

let output = harness.run(prompt, output_schema).await?;
```

Provide `MODEL_API_KEY`, a repository root, explicitly allowed tools, a prompt, and an
`OutputSchema`. Expect schema-validated JSON or a typed error.

Use `ModelWithMetadata::complete_with_metadata()` for normalized completion metadata.
