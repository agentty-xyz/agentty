//! Shared requests, recorded fixtures, and golden-request checks of the
//! per-protocol conformance suites.
//!
//! The golden `requests/` fixtures were recorded from the closed provider
//! enum this crate replaced, at `main` commit `7ddd0c382` (the base of the
//! `ProviderSpec` change), with key `test-key`, or the `plan-token`
//! credential and no key for `chatgpt`. To record them again, check out that
//! commit, copy this file to `crates/ag-router/tests/support/`, add the
//! recorder below as `crates/ag-router/tests/record_golden.rs` (with the
//! `PlanToken` credential the responses suite defines, implementing
//! `BearerCredential`), run it with `GOLDEN_OUT` naming an output directory,
//! and compare the output with the `requests/` files byte for byte, except
//! that the Chat Completions fixtures (every one but `chatgpt-*`, `openai-*`,
//! and `anthropic-*`) have since gained `"stream":true` and
//! `"stream_options":{"include_usage":true}` in their bodies, with their
//! `content-length` 54 bytes longer, since those requests stream, and the
//! `chatgpt-*` fixtures have since gained the `session-id` header carrying
//! their prompt cache key. The `anthropic-*` fixtures postdate that enum:
//! they record the first Anthropic Messages requests, written by hand from
//! the Messages API reference and
//! then matched to the serialized key order; they have since gained
//! `"stream":true` and a `"cache_control":{"type":"ephemeral"}` breakpoint
//! on the tool, the `system` block, and the last message's last block, with
//! their `content-length` 125 bytes longer. The `claude-fable-5-2`,
//! `claude-sonnet-5-5`, and `claude-haiku-5-5` fixtures were written the
//! same way after that; `claude-fable-5-2`, whose thinking-binding check
//! Anthropic enforces for every account, also carries the
//! `thinking-binding-controls-2026-08-01` `anthropic-beta` header and a
//! `"block_binding":{"prefix_mismatch_behavior":"drop_block"}` thinking
//! field. The `anthropic_messages/`
//! responses follow the same reference:
//!
//! ```text
//! #[allow(dead_code)]
//! #[path = "support/conformance.rs"]
//! mod conformance;
//!
//! #[tokio::test]
//! async fn record() -> Result<(), Box<dyn Error>> {
//!     let out = std::env::var("GOLDEN_OUT")?;
//!     // (fixture, model, with_tools, options): the cases of both suites'
//!     // `presets_send_byte_identical_requests`, with tools on every
//!     // Responses case.
//!     for (fixture, model, with_tools, options) in cases {
//!         let server = MockServer::start().await;
//!         Mock::given(method("POST"))
//!             .respond_with(ResponseTemplate::new(400).set_body_string("{}"))
//!             .mount(&server)
//!             .await;
//!         let (provider, api_key, credential) = match model.split_once('/').ok_or("id")?.0 {
//!             "kimi" => (Provider::Kimi, Some("test-key".to_string()), None),
//!             "muse" => (Provider::Muse, Some("test-key".to_string()), None),
//!             "qwen" => (Provider::Qwen, Some("test-key".to_string()), None),
//!             "openai" => (Provider::OpenAi, Some("test-key".to_string()), None),
//!             "chatgpt" => (Provider::ChatGpt, None, Some(Arc::new(PlanToken) as _)),
//!             _ => (Provider::OpenAiCompatible, Some("test-key".to_string()), None),
//!         };
//!         let router = Router::single(ProviderConfig {
//!             provider,
//!             api_key,
//!             base_url: format!("{}/v1", server.uri()),
//!             credential,
//!         });
//!         let _ = router
//!             .execute(conformance::golden_request(model, with_tools, options)?)
//!             .await;
//!         let request = conformance::only_request(&server).await?;
//!         let mut headers = serde_json::Map::new();
//!         for name in request.headers.keys() {
//!             if name.as_str() != "host" {
//!                 let value = request.headers.get(name).ok_or("header")?;
//!                 headers.insert(name.as_str().to_string(), json!(value.to_str()?));
//!             }
//!         }
//!         let body: Value = serde_json::from_slice(&request.body)?;
//!         let record = json!({
//!             "path": request.url.path(),
//!             "query": request.url.query(),
//!             "headers": headers,
//!             "body": body,
//!         });
//!         std::fs::write(format!("{out}/{fixture}.json"), format!("{record}\n"))?;
//!     }
//!
//!     Ok(())
//! }
//! ```

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

/// Asserts that `request` matches the golden record named `fixture` under
/// `requests/`: the same path, no query, each header once with the same
/// value, and a byte-identical body. Each failure names the fixture.
///
/// The module documentation records where the fixtures come from and how to
/// record them again.
pub(crate) fn assert_golden(request: &Request, fixture: &str) -> Result<(), Box<dyn Error>> {
    let path = format!(
        "{}/tests/fixtures/conformance/requests/{fixture}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let golden: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let mut headers = serde_json::Map::new();
    for name in request.headers.keys() {
        if name.as_str() != "host" {
            let mut values = request.headers.get_all(name).iter();
            let value = values.next().ok_or("header without a value")?;
            assert!(
                values.next().is_none(),
                "{fixture}: header `{name}` sent twice"
            );
            headers.insert(name.as_str().to_string(), json!(value.to_str()?));
        }
    }

    assert_eq!(json!(request.url.path()), golden["path"], "{fixture}: path");
    assert_eq!(
        json!(request.url.query()),
        golden["query"],
        "{fixture}: query"
    );
    assert_eq!(
        Value::Object(headers),
        golden["headers"],
        "{fixture}: headers"
    );
    assert_eq!(
        String::from_utf8(request.body.clone())?,
        serde_json::to_string(&golden["body"])?,
        "{fixture}: body"
    );

    Ok(())
}
