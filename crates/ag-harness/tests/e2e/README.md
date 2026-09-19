# Manual `ag-harness` E2E checks

These ignored tests make live requests to model providers and are excluded from normal
workspace test execution. Run them manually from the repository root with the required
provider credentials.

## Kimi

```sh
KIMI_API_KEY=... \
KIMI_BASE_URL=... \
KIMI_MODEL=... \
cargo test --locked -p ag-harness --test e2e kimi::test_kimi -- --exact --ignored --nocapture
```

## Muse

`MODEL_API_BASE_URL` defaults to `https://api.meta.ai/v1`, and `MODEL_API_MODEL`
defaults to `muse-spark-1.3`.

```sh
MODEL_API_KEY=... \
cargo test --locked -p ag-harness --test e2e muse::test_muse -- --exact --ignored --nocapture
```

The `muse-read` check lets Muse read this package's manifest and return its package
name. `AG_HARNESS_GIT_EXECUTABLE` must be an absolute path to a host-controlled Git
executable outside the worktree:

```sh
MODEL_API_KEY=... \
AG_HARNESS_GIT_EXECUTABLE=/absolute/path/to/git \
cargo test --locked -p ag-harness --test e2e muse_read::test_muse_read -- --exact --ignored --nocapture
```

## Qwen

```sh
DASHSCOPE_API_KEY=... \
DASHSCOPE_BASE_URL=... \
cargo test --locked -p ag-harness --test e2e qwen::test_qwen -- --exact --ignored --nocapture
```

## Images

The `image` checks send ordered PNG and JPEG blocks through one-shot and durable turns
for every built-in configuration that enables image input. Each follow-up must answer
from replayed image history. They also confirm that `qwen3-max`, which accepts image
parts but invents their content, is rejected before any request, and that image history
switches from Qwen VL to Muse but not to a text-only registration. They use the provider
variables above; run the Kimi checks one at a time when the account rate limit is low:

```sh
cargo test --locked -p ag-harness --test e2e image:: -- --test-threads=1 --ignored --nocapture
```

Run all live-provider checks together only when every provider credential is configured:

```sh
cargo test --locked -p ag-harness --test e2e -- --test-threads=1 --ignored --nocapture
```
