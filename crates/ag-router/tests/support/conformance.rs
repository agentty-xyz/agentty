//! Shared requests, recorded fixtures, and golden-request checks of the
//! per-protocol conformance suites.
//!
//! The golden `requests/` fixtures were recorded from `main` at commit
//! `690dc10d7`, before the `ProviderSpec` change; see [`assert_golden`].

use std::error::Error;
use std::num::NonZeroU32;

use ag_router::{
    GenerationOptions, JsonSchemaFormat, ModelMessage, ModelRequest, OutputSchema, ReasoningEffort,
    ToolCall, ToolDefinition,
};
use serde_json::{Value, json};
use wiremock::{MockServer, Request};

/// Returns the person schema every conformance request asks for.
pub(crate) fn format() -> Result<JsonSchemaFormat, Box<dyn Error>> {
    Ok(JsonSchemaFormat::new(
        "person",
        OutputSchema::new(json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"],
            "additionalProperties": false
        }))?,
    )?)
}

/// Returns a structured request whose history exercises every replayed
/// message kind a golden request records: system text, user text, a tool
/// call with reasoning, its result, and turn instructions.
pub(crate) fn golden_request(
    model: &str,
    with_tools: bool,
    options: GenerationOptions,
) -> Result<ModelRequest, Box<dyn Error>> {
    let call = ToolCall::from_json(
        "call_1".to_string(),
        "read",
        r#"{"path":"a.txt"}"#,
        Some("thinking about a".to_string()),
    )?;
    let tools = if with_tools {
        vec![ToolDefinition::new(
            "read",
            "Read one file.",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        )]
    } else {
        Vec::new()
    };
    let mut request = ModelRequest::chat(
        model,
        vec![
            ModelMessage::System("You extract names.".to_string()),
            ModelMessage::User("Find the name".to_string()),
            ModelMessage::AssistantToolCall(call),
            ModelMessage::ToolResult {
                call_id: "call_1".to_string(),
                content: r#"{"text":"Ada"}"#.to_string(),
                name: "read".to_string(),
            },
            ModelMessage::Instructions("Answer briefly.".to_string()),
        ],
        tools,
        format()?,
    );
    request.options = options;

    Ok(request)
}

/// Returns generation options with an optional effort, the `session-1`
/// prompt cache key when `cache_key` is set, and an optional bound.
pub(crate) fn options(
    effort: Option<ReasoningEffort>,
    cache_key: bool,
    max_output_tokens: Option<u32>,
) -> GenerationOptions {
    GenerationOptions {
        max_output_tokens: max_output_tokens.and_then(NonZeroU32::new),
        prefix_schema: None,
        prompt_cache_key: cache_key.then(|| "session-1".to_string()),
        reasoning_effort: effort,
    }
}

/// Returns the one request `server` received.
pub(crate) async fn only_request(server: &MockServer) -> Result<Request, Box<dyn Error>> {
    let mut requests = server
        .received_requests()
        .await
        .ok_or("request recording disabled")?;
    if requests.len() != 1 {
        return Err(format!("expected one request, received {}", requests.len()).into());
    }

    Ok(requests.remove(0))
}

/// Asserts that `server` received no request.
pub(crate) async fn assert_no_request(server: &MockServer) -> Result<(), Box<dyn Error>> {
    let requests = server
        .received_requests()
        .await
        .ok_or("request recording disabled")?;
    assert!(requests.is_empty(), "received {} requests", requests.len());

    Ok(())
}

/// Returns the display of `error` and every source, joined by `: `.
pub(crate) fn error_chain(error: &(dyn Error + 'static)) -> String {
    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        chain.push_str(": ");
        chain.push_str(&error.to_string());
        source = error.source();
    }

    chain
}

/// Asserts that `request` matches the golden record `fixture`: the same
/// path, no query, each header once with the same value, and a
/// byte-identical body.
///
/// The `requests/` fixtures were recorded from the closed provider enum
/// this crate replaced, at `main` commit `690dc10d7` (the base of the
/// `ProviderSpec` change), by sending the same golden requests through
/// `Router::single(ProviderConfig { provider, api_key, base_url,
/// credential })` with key `test-key`, or the `plan-token` credential and
/// no key for `chatgpt`. To repeat the check, check out that commit and run
/// those cases against these fixtures with this helper.
pub(crate) fn assert_golden(request: &Request, fixture: &str) -> Result<(), Box<dyn Error>> {
    let golden: Value = serde_json::from_str(fixture)?;
    let mut headers = serde_json::Map::new();
    for name in request.headers.keys() {
        if name.as_str() != "host" {
            let mut values = request.headers.get_all(name).iter();
            let value = values.next().ok_or("header without a value")?;
            assert!(values.next().is_none(), "header `{name}` sent twice");
            headers.insert(name.as_str().to_string(), json!(value.to_str()?));
        }
    }

    assert_eq!(json!(request.url.path()), golden["path"]);
    assert_eq!(json!(request.url.query()), golden["query"]);
    assert_eq!(Value::Object(headers), golden["headers"]);
    assert_eq!(
        String::from_utf8(request.body.clone())?,
        serde_json::to_string(&golden["body"])?
    );

    Ok(())
}
