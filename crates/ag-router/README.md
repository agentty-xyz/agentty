# `ag-router`

One Rust API for structured chat requests to Muse, Kimi, Qwen, `OpenAI` (`openai`,
through the Responses API, or `chatgpt`, billed to a ChatGPT plan), Anthropic Claude
(`anthropic`, through the Messages API), and any OpenAI-compatible Chat Completions
server (`openai-compatible`), such as Ollama, llama.cpp, LM Studio, or vLLM. The host
supplies provider credentials and chooses the model explicitly as `provider/model`. The
model component may itself contain `/`. Every request requires a named JSON Schema
response format. The router returns locally validated JSON or generic function calls for
the caller to execute.

Each provider is a `ProviderSpec`: its `WireProtocol` (`ChatCompletions`, `Responses`,
or `AnthropicMessages`), base URL, `Auth` (none, a bearer key, a named header, or a
refreshing `CredentialSource`), extra headers and query parameters, and request policy:
structured-output mode, reasoning mapping, reasoning-text replay, output-bound field,
image input, prompt cache key, and context-overflow markers. `ProviderSpec::new` starts
from the standard request for the protocol with no optional capabilities, so a host adds
an OpenAI-compatible, Responses, or Anthropic Messages provider without code changes.
Provider quirks are opt-ins: the legacy `max_tokens` field (`with_max_tokens_field`; the
default is the current `OpenAI` field `max_completion_tokens`, which some compatible
servers ignore, leaving output unbounded), turn instructions joined to the preceding
user message (`with_instruction_placement(InstructionPlacement::JoinPreviousUser)`),
reasoning-text replay, and tool-result names. Each Chat Completions preset states its
output-bound field and enables the quirks its provider needs. The built-in providers are
`preset`s routed as `anthropic`, `chatgpt`, `kimi`, `muse`, `openai`,
`openai-compatible`, and `qwen`; every preset except `openai_compatible` is tailored to
one model and serves only that model (`ProviderSpec::for_model`). A router routes
`provider/model` to the specification for that model, else to the provider's
model-independent one, so it can hold one preset per model; a model no specification
serves fails with `ModelError::NoProviderSpec`. Of duplicate specifications the router
keeps the first in every build; `Router::with_override` replaces one.
`ProviderSpec::validate` rejects a specification no request can be sent with, and every
request runs it before any network access, failing with
`ModelError::InvalidProviderConfig` naming the field. Header and query values, and
credentials or a query in the base URL, are never printed by debug output or errors, and
requests follow no redirect, so keys never reach another host.

```rust
use ag_router::{
    Completion, JsonSchemaFormat, ModelMessage, ModelRequest, OutputSchema, Router, preset,
};
use serde_json::json;

let router = Router::new([preset::muse(
    "https://api.meta.ai/v1",
    std::env::var("MODEL_API_KEY")?,
    "muse-spark-1.3",
)]);
let format = JsonSchemaFormat::new(
    "answer",
    OutputSchema::new(json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"],
        "additionalProperties": false
    }))?,
)?;
let request = ModelRequest::chat(
    "muse/muse-spark-1.3",
    vec![ModelMessage::User("What is Rust?".to_string())],
    vec![],
    format,
);

match router.execute(request).await? {
    Completion::Output { value, .. } => println!("{}", value["answer"]),
    Completion::ToolCalls { calls, .. } => {
        // Execute allowed functions in the host, then send their results in the next request.
        println!("{} function calls", calls.len());
    }
}
```

The public response contract is always JSON Schema. Muse receives native `json_schema`
wire requests; Kimi and Qwen use their supported `json_object` mode plus a schema
instruction, then all three undergo the same local validation. Tool calls are
intermediate responses and are never executed by this crate. Text a model writes beside
its calls is kept on the first call (`ToolCall::assistant_text`) and replayed as that
assistant message's `content`. An optional `GenerationOptions::prompt_cache_key` is sent
as `prompt_cache_key` to Muse and Kimi, which route requests sharing a key to the
backend holding their cached prefix; Qwen requests leave it off. A request that asks for
another schema than the conversation it continues sets
`GenerationOptions::prefix_schema` to the conversation's schema: Kimi and Qwen keep
stating that one in their leading schema instruction, so the request keeps the cached
prefix (`Router::keeps_prefix_across_schemas`), and the caller states the requested
schema after the conversation; Muse receives the requested schema natively. Provider
selection is explicit; routing does not retry a request on another provider.

Each specification retries failed requests under its `RetryPolicy`
(`ProviderSpec::with_retry`). The default, which every preset keeps, retries a `429`
without a spent-quota marker up to five times and one connection failure, waiting an
exponential backoff from one second capped at five, or the provider's `Retry-After`
(delta-seconds or an HTTP date, capped at sixty seconds) when longer. `with_retried`
opts in server errors (`500`, `502`-`504`, `RetryKind::ServerError`) and an overloaded
`529` (`RetryKind::Overloaded`); the builder also sets the attempts, base delay, caps,
and `Jitter`. A spent quota (`402`, or a `429` reporting one) and a timed-out request
are never retried. `Router::execute_observed` reports each retry to a `RetryObserver`
before its wait, and dropping the request future stops a pending wait.

Responses and Chat Completions requests stream (`stream: true`). Chat Completions also
asks for usage with `stream_options.include_usage`, which every preset keeps;
`ProviderSpec::with_stream_usage(false)` turns it off for a server that rejects it. A
stream may run for 30 minutes in total but fails after 5 minutes of silence. A Chat
Completions server that ignores `stream` and answers with one JSON body is read as
before. `Router::execute_streamed` reports each `ResponseDelta` (text, reasoning, or a
call's arguments by its position) to a `DeltaObserver` as it arrives. The returned
completion is decoded exactly as an unstreamed response would be, so deltas only preview
it. A failure reported inside a stream is classified like an HTTP rejection, and a
stream that ends before its response finishes is a transport failure. Anthropic Messages
requests are unstreamed and report no deltas.

`openai-compatible` applies a conservative policy for servers whose extensions are
unknown: the API key is optional (`None` sends no `Authorization` header), requests
carry only `model`, `messages`, `tools`, `response_format`, and the legacy `max_tokens`,
with no `prompt_cache_key`, reasoning controls, or image input, and turn instructions
join the preceding user message for chat templates that require alternating roles. A
server that supports more opts in with the preset's `with_*` methods, such as
`with_image_input(true)` or `JsonSchema` structured output. The schema is stated in a
leading system message like Kimi and Qwen, worded to invite tool calls before the final
JSON answer because small local models read an unconditional "return only JSON" as
forbidding them; requests without tools also send the `json_schema` response format,
which Ollama, llama.cpp, LM Studio, and vLLM all accept. Reasoning a server returns is
not replayed, so the history stays portable. Context overflow is recognized from the
shared `OpenAI` markers and llama.cpp's `exceed_context_size_error`. `LocalServer` reads
such a server's model inventory (`LocalServer::inventory`: Ollama's `/api/tags` and
`/api/show` with context windows, tool support, and loaded windows from `/api/ps`, else
`GET {base}/models`) under two-second request timeouts, and the window the server runs a
model with (`LocalServer::running_context_window`), loading an unloaded Ollama model
first because Ollama reports the window only for loaded models.

`openai` sends requests to `{base_url}/responses`, the only `OpenAI` API that offers
function tools to the current GPT models. Requests are stateless (`store: false`) and
streamed; the router reads the server-sent events and returns one completion, so callers
see the same contract as every other provider. System messages travel as `developer`
messages, the schema is a native but non-strict `json_schema` text format beside
non-strict function tools, `parallel_tool_calls` is on, and `prompt_cache_key` is sent.
The reasoning items of a response are returned as one JSON array in the reasoning
content of its first call or output, and replayed verbatim before that assistant turn;
reasoning content that is not such an array is omitted. Usage reports cached input
tokens. An overflow is recognized from an HTTP rejection or from a streamed
`response.failed`, a response cut short reports its `incomplete_details` reason, and a
stream that closes before its terminal event is a transport failure. Output text,
reasoning summary, and function-argument deltas reach a `DeltaObserver`. The current
`Task` surface supports chat only; other model modes are not yet implemented.

`anthropic` sends unstreamed requests to `{base_url}/messages` with the key in
`x-api-key` and the protocol's `anthropic-version: 2023-06-01` header, which
`with_header` replaces. System messages become top-level `system` blocks, consecutive
messages of one role merge, tool results return as `tool_result` blocks, and images as
base64 `image` blocks. A request without an output bound sends `max_tokens` 16,000
(`ProviderSpec::with_fallback_max_tokens`), since the API requires one. The schema is
the native `output_config.format`, or with `StructuredOutputMode::ForcedTool` a
`structured_output` tool whose input is the answer, forced only for a specification
without a reasoning mapping, since thinking forbids a forced tool. A reasoning effort is
sent as `output_config.effort` with summarized adaptive thinking. A response's signed
`thinking` and `redacted_thinking` blocks are returned as one JSON array in the
reasoning content of its first call or output, with markers where its text and calls
went, and replayed verbatim in that order; other reasoning content is omitted. Signed
thinking survives only history that grows, so `Router::replays_turn_instructions` tells
callers to replay earlier turns' instructions as first sent. Usage counts cache reads
and writes as input and reports the writes. A `max_tokens`, `pause_turn`, or `refusal`
stop is an incomplete response, a `model_context_window_exceeded` stop or a "prompt is
too long" `400` is a context overflow, and an HTTP `529` keeps its status for the
caller's overload handling.

`chatgpt` sends the same Responses requests billed to the user's ChatGPT plan through
OpenAI's "Sign in with ChatGPT" instead of API credits. Its preset authenticates with
`Auth::Credential`, a `CredentialSource` the router asks for a token before each
request. `chatgpt::ChatGptLogin` runs the browser sign-in (PKCE against
`auth.openai.com`, registering the app as "Agentty" on first use and reusing that
`client_id` afterwards) and `chatgpt::ChatGptAuth` reads the stored
`chatgpt::ChatGptCredentials`, refreshing the one-hour access token five minutes before
it expires. Refresh tokens rotate, so `ChatGptAuth` serializes refreshes within the
process and, through an exclusive lock on a sibling `.lock` file, across processes, and
a terminal refresh error asks the user to sign in again. A credential may replace the
API key of any provider.
