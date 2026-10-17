# Real-model compatibility benchmark

This manual benchmark exercises `ag-harness` against every supported live provider. It
is a small system benchmark, not an official leaderboard submission. Each case has a
deterministic scorer: schema validation, exact tool activity, or final filesystem state.
Expected answers are deliberately absent from tool and memory schemas so models cannot
solve a case by copying constraints instead of using tools or persisted history.

## Benchmark coverage

| Public benchmark   | Applicable case                      | Scope boundary                                                                                                                                                                                                                                                  |
| ------------------ | ------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| JSONSchemaBench    | `structured`                         | Exercises a nested object, constants, and a fixed tuple. The official 10K-schema constrained-decoding benchmark is not run because `ag-harness` intentionally accepts only explicit object-root response schemas and providers impose their own schema subsets. |
| BFCL               | `parallel_read`                      | Scores exact batched function selection and arguments. Full BFCL needs arbitrary user-defined functions, parallel execution, and categories outside the closed repository-tool surface.                                                                         |
| tau-bench          | `read_recovery`, `persistent_memory` | Scores multi-step recovery after a rejected tool and cross-process-style state restored from SQLite. Full tau-bench needs domain APIs, policy documents, and a simulated user.                                                                                  |
| GAIA               | `parallel_read`                      | Covers bounded local-file retrieval only. Full GAIA needs web, Python, and shell tools plus its gated validation data.                                                                                                                                          |
| SWE-bench Verified | `write`                              | Verifies the resulting file rather than trusting the terminal answer. Full SWE-bench needs per-task repositories, dependency installation, shell execution, and test-based patch scoring.                                                                       |

EleutherAI's LM Evaluation Harness, OpenAI Evals, and Inspect AI are evaluation
frameworks rather than additional task datasets. Their model adapters and task catalogs
are useful integration targets, but an adapter alone would not expand what this crate's
fixed tool surface can validly measure.

## Run

Configure `KIMI_API_KEY`, `KIMI_BASE_URL`, `KIMI_MODEL`, `MODEL_API_KEY`,
`DASHSCOPE_API_KEY`, and `DASHSCOPE_BASE_URL`. Repository-backed cases also require
`AG_HARNESS_GIT_EXECUTABLE` to be an absolute path to a host-controlled Git executable
outside the worktree. To run the full benchmark:

```sh
AG_HARNESS_GIT_EXECUTABLE=/absolute/path/to/git \
AG_HARNESS_BENCHMARK_REPETITIONS=2 \
  cargo test --locked -p ag-harness --test benchmark
```

Set `AG_HARNESS_BENCHMARK_PROVIDER` to `kimi`, `muse`, or `qwen`, and
`AG_HARNESS_BENCHMARK_CASE` to one case name for targeted reruns.

`MODEL_API_BASE_URL`, `MODEL_API_MODEL`, and `QWEN_MODEL` (default `qwen-plus`) remain
optional. Kimi cases use a full-minute cooldown to respect its three-request-per-minute
organization limit. Results stream after every case and record pass/fail, wall time,
summed provider and turn latency, SQLite create/reopen latency for `persistent_memory`,
successful model requests, executed tool calls, and provider-reported total tokens.
Provider error bodies are redacted. Failed cases report zero activity because the public
turn report is available only after a successful turn.

The benchmark captures OpenTelemetry metrics and spans in-process. It verifies client
duration/token metrics plus lifecycle duration/call metrics, and requires one correlated
agent and model span for every persistent-session turn. It does not export telemetry,
estimate missing tokens, or calculate cost.

See `results/2026-09-03.md` for the completed live persistence and compatibility report.

## Context suite

The context suite measures in-turn context management at a forced small `ContextBudget`.
Each scenario runs as one `run_once` turn against a committed Git fixture generated from
a fixed seed, so every run reads the same bytes:

| Scenario             | Task                                                                                                  | Correctness checks                                     |
| -------------------- | ----------------------------------------------------------------------------------------------------- | ------------------------------------------------------ |
| `long_read_loop`     | Read 16 varied Rust-like files and list each file's `pub fn` names plus the file with the most.       | One per file (exact name set) plus the maximum file.   |
| `doc_comment_writes` | Apply three named doc-comment edits across two files of about 250 lines; one target is past line 200. | One per edit plus the exact final contents and status. |
| `huge_result`        | Read six small notes, then one 48 KB single-line file; return all seven code words in order.          | One per code word.                                     |

Select it with `AG_HARNESS_BENCHMARK_SUITE=context` and exactly one provider per
process. Set `QWEN_MODEL` to compare Qwen models:

```sh
AG_HARNESS_GIT_EXECUTABLE=/absolute/path/to/git \
AG_HARNESS_BENCHMARK_SUITE=context \
AG_HARNESS_BENCHMARK_PROVIDER=qwen \
QWEN_MODEL=qwen3.8-max \
AG_HARNESS_BENCHMARK_REPETITIONS=2 \
  cargo test --locked -p ag-harness --test benchmark
```

`AG_HARNESS_BENCHMARK_CASE` selects one scenario. `AG_HARNESS_CONTEXT_WINDOW` (default
`32000`) and `AG_HARNESS_CONTEXT_RESERVED_OUTPUT` (default `16384`) set the budget.
`AG_HARNESS_CONTEXT_TIMEOUT_SECS` (default `900`) bounds each run; a run that exceeds it
is recorded as failed. `AG_HARNESS_CONTEXT_SUMMARIES=1` enables
`ContextSummaryPolicy::Enabled`, so outgrown requests summarize earlier tool rounds.
Kimi is not paced in this suite, so its three-request-per-minute limit makes it
unsuitable without a higher quota.

Each run appends one JSON object to `AG_HARNESS_CONTEXT_RESULTS` (default
`target/ag-harness-context-benchmark.jsonl`) and prints it after `CONTEXT`:

- `success` means every check passed; `correctness` is the passed fraction. Both are
  derived from the final output and worktree, never from the answer text alone.
- `model_calls`, `output_corrections`, `malformed_tool_calls`, `repetition_nudges`,
  `context_overflow_recoveries`, and `tool_calls` come from the turn report;
  `model_calls` includes the `summary_calls` that summarized earlier rounds, and
  `summary_failures` counts summaries that failed or were rejected.
- `summary_outcomes` and `summary_ms` list each summary's outcome (`completed` or its
  `ContextSummaryErrorType` value) and duration, and `failed_requests` lists every
  failed model request as its error type with any HTTP status (`transport_error`,
  `provider_error:429`). These come from lifecycle events, so a failed run keeps them.
- `duplicate_reads` counts reads repeating an earlier file read with the same path and
  starting line, or an identical `list`/`search` inspection.
- `input_tokens` and `output_tokens` sum provider-reported usage; `null` means the
  provider reported none.
- `requests` lists every accepted request in order: its `purpose` (`turn` or
  `context_summary`), `ms`, provider-reported `input_tokens`, `cache_hit_tokens`,
  `output_tokens`, `reasoning_tokens`, and `finish_reason`, the harness's
  `estimated_weight` and `calibrated_weight`, and its non-zero `reductions`.
- `elided_results`, `cleared_reasoning`, `shrunk_write_calls`, `dropped_history_turns`,
  `dropped_tool_calls`, `truncated_results`, `shrunk_latest_write_calls`,
  `summarized_tool_calls`, and `superseded_results` sum `ContextActivity` over every
  accepted request. A reduction carried into later requests is counted once per request,
  so these measure sustained pressure rather than distinct items.
- `revision`, `model`, `window`, `reserved_output`, and `context_summaries` identify the
  configuration. A failed turn's activity fields come from the report on its
  `TurnFailed` event; they are `null` only when the turn failed before preparing its
  first request or timed out. `detail` holds the error or failed checks.

To compare runs, aggregate per scenario and model with `jq`:

```sh
jq -s -r 'group_by([.scenario, .model, .context_summaries])[] | {
    scenario: .[0].scenario, model: .[0].model, runs: length,
    summaries: .[0].context_summaries,
    success: (map(select(.success)) | length),
    calls: (map(.model_calls // empty) | add / length),
    duplicates: (map(.duplicate_reads // empty) | add / length),
    input_tokens: (map(.input_tokens // empty) | add / length),
    wall_s: (map(.wall_ms) | add / length / 1000)
  } | [.scenario, .model, .summaries, "\(.success)/\(.runs)", .calls, .duplicates,
       .input_tokens, .wall_s] | @tsv' results.jsonl
```

Committed baselines live under `results/` as `*-context.jsonl`.
