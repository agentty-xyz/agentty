# `ag-router`

One Rust API for structured chat requests to Muse, Kimi, Qwen, and any OpenAI-compatible
Chat Completions server (`openai-compatible`), such as Ollama, llama.cpp, LM Studio, or
vLLM. The host supplies provider credentials and chooses the model explicitly as
`provider/model`. The model component may itself contain `/`. Every request requires a
named JSON Schema response format. The router returns locally validated JSON or generic
function calls for the caller to execute.

```rust
use ag_router::{
    Completion, JsonSchemaFormat, ModelMessage, ModelRequest, OutputSchema, Provider,
    ProviderConfig, Router,
};
use serde_json::json;

let router = Router::new([ProviderConfig {
    provider: Provider::Muse,
    api_key: Some(std::env::var("MODEL_API_KEY")?),
    base_url: "https://api.meta.ai/v1".to_string(),
}])?;
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

`openai-compatible` applies a conservative policy for servers whose extensions are
unknown: the API key is optional (`None` sends no `Authorization` header), requests
carry only `model`, `messages`, `tools`, and `response_format`, with no
`prompt_cache_key`, reasoning controls, or image input. The schema is stated in a
leading system message like Kimi and Qwen, worded to invite tool calls before the final
JSON answer because small local models read an unconditional "return only JSON" as
forbidding them; requests without tools also send the `json_schema` response format,
which Ollama, llama.cpp, LM Studio, and vLLM all accept. Reasoning a server returns is
not replayed, so the history stays portable. Context overflow is recognized from the
shared `OpenAI` markers and llama.cpp's `exceed_context_size_error`. The current `Task`
surface supports chat only. Streaming and other model modes are not yet implemented.
