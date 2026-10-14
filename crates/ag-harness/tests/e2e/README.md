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

The feature checks in `features.rs` run with `qwen-plus` unless `QWEN_MODEL` names
another Qwen model.

## Malformed tool arguments

`features::test_malformed_tool_arguments` answers the first request with a malformed
`read` call, then lets the live model continue. The provider must accept the replayed
call and its error result, the model must re-issue a valid read, and a second durable
turn must replay the stored malformed call. Like the other feature checks, it runs Git
from `/opt/homebrew/bin/git` and uses the provider variables above; `AG_LIVE_PROVIDERS`
selects providers:

```sh
AG_LIVE_PROVIDERS=muse,qwen \
cargo test --locked -p ag-harness --test e2e features::test_malformed_tool_arguments -- --exact --ignored --nocapture
```

## Context overflow

`features::test_context_overflow_classification` sends one turn of random words beyond
the model's window under an unbounded `ContextBudget`, so the provider rather than the
harness rejects it, and requires `ModelErrorType::ContextOverflow`. Kimi's input stays
below its two-million tokens-per-minute limit, which a larger input meets as a `429`
first:

```sh
AG_LIVE_ROUNDS=1 \
cargo test --locked -p ag-harness --test e2e features::test_context_overflow_classification -- --exact --ignored --nocapture
```

## Repeated tool calls

`features::test_repeated_tool_calls` injects three identical `read` calls before the
live model takes over, so the provider must accept the request-only nudge that follows
their results and the model must answer from what it already read. A second case asks
the live model to repeat one read six times; when it does, the harness must nudge it or
stop the turn with `RepeatedToolCalls`, and a model that declines to repeat is reported
as inconclusive:

```sh
AG_LIVE_PROVIDERS=muse,qwen \
cargo test --locked -p ag-harness --test e2e features::test_repeated_tool_calls -- --exact --ignored --nocapture
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
