# `ag-router`

One Rust API for structured chat requests to Muse, Kimi, and Qwen. The host supplies
provider credentials and chooses the model explicitly as `provider/model`. The model
component may itself contain `/`. Every request requires a named JSON Schema response
format. The router returns locally validated JSON or generic function calls for the
caller to execute.

```rust
use ag_router::{
    Completion, JsonSchemaFormat, ModelMessage, ModelRequest, OutputSchema, Provider,
    ProviderConfig, Router,
};
use serde_json::json;

let router = Router::new([ProviderConfig {
    provider: Provider::Muse,
    api_key: std::env::var("MODEL_API_KEY")?,
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
intermediate responses and are never executed by this crate. Provider selection is
explicit; routing does not retry a request on another provider. The current `Task`
surface supports chat only. Streaming and other model modes are not yet implemented.
