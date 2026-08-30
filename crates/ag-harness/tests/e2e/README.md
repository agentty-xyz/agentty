# Manual `ag-harness` E2E checks

These ignored tests make live requests to model providers and are excluded from normal
workspace test execution. Run them manually from the repository root with the required
provider credentials.

## Codex

Authenticate the `codex` executable with ChatGPT first. This routing smoke test probes
`gpt-5.6-luna` with the `ag-harness` originator; model availability depends on the
account and is not implied by API availability.

```sh
cargo test --locked -p ag-harness --test e2e codex::test_codex_luna_with_ag_harness_originator -- --exact --ignored --nocapture
```

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
name:

```sh
MODEL_API_KEY=... \
cargo test --locked -p ag-harness --test e2e muse_read::test_muse_read -- --exact --ignored --nocapture
```

## Qwen

```sh
DASHSCOPE_API_KEY=... \
DASHSCOPE_BASE_URL=... \
cargo test --locked -p ag-harness --test e2e qwen::test_qwen -- --exact --ignored --nocapture
```

Run all live-provider checks together only when every provider credential is configured:

```sh
cargo test --locked -p ag-harness --test e2e -- --test-threads=1 --ignored --nocapture
```
