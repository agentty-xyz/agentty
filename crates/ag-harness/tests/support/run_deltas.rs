//! Public streaming contract of built-in clients: a streamed turn response
//! opens its reasoning, message, and tool-call items at their first delta,
//! streams `ItemDelta` events into them, and completes them afterwards, on
//! the Chat Completions and Anthropic Messages protocols alike.

use std::error::Error;
use std::fmt::Write as _;
use std::future::poll_fn;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use ag_harness::event::{Delta, Item, ItemKind};
use ag_harness::model::ModelClient;
use ag_harness::provider::{ProviderSpec, WireProtocol};
use ag_harness::{
    EventStream, Harness, OutputMode, OutputSchema, RunEvent, RunEventKind, RunOutput, RunRequest,
};
use futures_core::Stream;
use serde_json::{Value, json};
use tokio::time::timeout;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::context_budget_fixture::unbounded_context_budget;

/// Bounds waits that would hang if a run never resolved.
const HANG: Duration = Duration::from_secs(30);

/// One event reduced to what the streaming contract orders.
#[derive(Debug, PartialEq)]
enum Seen {
    Started(u64, ItemKind),
    Delta(u64, Delta),
    Completed(u64, Item),
    /// A completed tool-call item, whose result the tool decides.
    CallCompleted(u64),
    Request,
}

impl Seen {
    fn text(id: u64, text: &str) -> Self {
        Self::Delta(
            id,
            Delta::Text {
                text: text.to_string(),
            },
        )
    }

    fn reasoning(id: u64, text: &str) -> Self {
        Self::Delta(
            id,
            Delta::Reasoning {
                text: text.to_string(),
            },
        )
    }

    fn arguments(id: u64, arguments: &str) -> Self {
        Self::Delta(
            id,
            Delta::ToolArguments {
                arguments: arguments.to_string(),
            },
        )
    }

    fn message(id: u64, text: &str) -> Self {
        Self::Completed(
            id,
            Item::Message {
                text: text.to_string(),
            },
        )
    }
}

/// Returns a harness whose Chat Completions model is served at `base_url`.
fn harness(base_url: String) -> Result<Harness, Box<dyn Error>> {
    let spec = ProviderSpec::new("gateway", WireProtocol::ChatCompletions, base_url)
        .for_model("gateway-model");

    Ok(Harness::builder()
        .model(Arc::new(ModelClient::from_spec(spec)?))
        .context_budget(unbounded_context_budget())
        .build()?)
}

fn chunk(delta: &Value) -> Value {
    json!({"choices": [{"index": 0, "delta": delta}]})
}

fn arguments(index: u64, id: Option<&str>, arguments: &str) -> Value {
    let mut call = json!({"index": index, "function": {"arguments": arguments}});
    if let Some(id) = id {
        call["id"] = json!(id);
        call["type"] = json!("function");
        call["function"]["name"] = json!("lookup");
    }

    chunk(&json!({"tool_calls": [call]}))
}

/// Mounts the next streamed answer of `server`: `chunks` ended by
/// `finish_reason` and `[DONE]`, sent once or, without `times`, always.
async fn mount(server: &MockServer, chunks: &[Value], finish_reason: &str, times: Option<u64>) {
    let mut body = String::new();
    for chunk in chunks {
        let _ = write!(body, "data: {chunk}\n\n");
    }
    let finished = json!({"choices": [{"index": 0, "delta": {}, "finish_reason": finish_reason}]});
    let _ = write!(body, "data: {finished}\n\ndata: [DONE]\n\n");
    let mock = Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"));
    match times {
        Some(times) => mock.up_to_n_times(times).mount(server).await,
        None => mock.mount(server).await,
    }
}

/// Waits for the next event through the `Stream` implementation.
async fn next_event(stream: &mut EventStream) -> Option<RunEvent> {
    poll_fn(|context| Pin::new(&mut *stream).poll_next(context)).await
}

/// Reads every item and request event of `stream`.
async fn drain(mut stream: EventStream) -> Vec<Seen> {
    let mut seen = Vec::new();
    while let Some(event) = next_event(&mut stream).await {
        seen.extend(match event.kind {
            RunEventKind::ItemStarted { id, item_kind } => Some(Seen::Started(id.get(), item_kind)),
            RunEventKind::ItemDelta { delta, id } => Some(Seen::Delta(id.get(), delta)),
            RunEventKind::ItemCompleted {
                id,
                item: Item::ToolCall(_),
            } => Some(Seen::CallCompleted(id.get())),
            RunEventKind::ItemCompleted { id, item } => Some(Seen::Completed(id.get(), item)),
            RunEventKind::ModelRequestCompleted { .. } => Some(Seen::Request),
            _ => None,
        });
    }

    seen
}

#[tokio::test]
async fn streams_deltas_into_the_items_each_response_completes() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = MockServer::start().await;
    let calls = [
        chunk(&json!({"reasoning_content": "Look "})),
        chunk(&json!({"reasoning_content": "it up."})),
        chunk(&json!({"content": "Checking."})),
        arguments(0, Some("call_a"), "{\"q\":"),
        arguments(0, None, "\"a\"}"),
        arguments(1, Some("call_b"), "{\"q\":\"b\"}"),
    ];
    mount(&server, &calls, "tool_calls", Some(1)).await;
    let answer = [
        chunk(&json!({"content": "Ada "})),
        chunk(&json!({"content": "Lovelace"})),
    ];
    mount(&server, &answer, "stop", None).await;
    let harness = harness(format!("{}/v1", server.uri()))?;

    // Act
    let request = RunRequest::new("Who?").output(OutputMode::Text);
    let (stream, handle) = harness.run(request).into_parts();
    let (seen, outcome) = timeout(HANG, async {
        tokio::join!(drain(stream), handle.outcome())
    })
    .await?;

    // Assert
    let output = RunOutput::Text("Ada Lovelace".to_string());
    assert_eq!(outcome.result.as_ref().ok(), Some(&output));
    assert_eq!(
        seen,
        [
            Seen::Started(0, ItemKind::Reasoning),
            Seen::reasoning(0, "Look "),
            Seen::reasoning(0, "it up."),
            Seen::Started(1, ItemKind::Message),
            Seen::text(1, "Checking."),
            Seen::Started(2, ItemKind::ToolCall),
            Seen::arguments(2, "{\"q\":"),
            Seen::arguments(2, "\"a\"}"),
            Seen::Started(3, ItemKind::ToolCall),
            Seen::arguments(3, "{\"q\":\"b\"}"),
            Seen::Request,
            // The default policy keeps no reasoning text, so the item
            // completes with the streamed text.
            Seen::Completed(
                0,
                Item::Reasoning {
                    text: "Look it up.".to_string()
                }
            ),
            Seen::message(1, "Checking."),
            Seen::CallCompleted(2),
            Seen::CallCompleted(3),
            Seen::Started(4, ItemKind::Message),
            Seen::text(4, "Ada "),
            Seen::text(4, "Lovelace"),
            Seen::Request,
            Seen::message(4, "Ada Lovelace"),
            Seen::Started(5, ItemKind::Output),
            Seen::Completed(5, Item::Output { output }),
        ]
    );

    Ok(())
}

#[tokio::test]
async fn completes_the_streamed_text_of_a_discarded_answer() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = MockServer::start().await;
    mount(
        &server,
        &[chunk(&json!({"content": "not json"}))],
        "stop",
        Some(1),
    )
    .await;
    let answer = [chunk(&json!({"content": "{\"name\":\"Ada\"}"}))];
    mount(&server, &answer, "stop", None).await;
    let harness = harness(format!("{}/v1", server.uri()))?;
    let schema = OutputSchema::new(json!({
        "type": "object",
        "properties": {"name": {"type": "string"}},
        "required": ["name"]
    }))?;

    // Act
    let (stream, handle) = harness
        .run(RunRequest::new("Who?").output(schema))
        .into_parts();
    let (seen, outcome) = timeout(HANG, async {
        tokio::join!(drain(stream), handle.outcome())
    })
    .await?;

    // Assert
    let output = RunOutput::Json(json!({"name": "Ada"}));
    assert_eq!(outcome.result.as_ref().ok(), Some(&output));
    let items: Vec<_> = seen
        .into_iter()
        .filter(|seen| *seen != Seen::Request)
        .collect();
    assert_eq!(
        items,
        [
            Seen::Started(0, ItemKind::Message),
            Seen::text(0, "not json"),
            Seen::message(0, "not json"),
            Seen::Started(1, ItemKind::Message),
            Seen::text(1, "{\"name\":\"Ada\"}"),
            Seen::message(1, "{\"name\":\"Ada\"}"),
            Seen::Started(2, ItemKind::Output),
            Seen::Completed(2, Item::Output { output }),
        ]
    );

    Ok(())
}

#[tokio::test]
async fn streams_anthropic_thinking_and_text_into_items() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = MockServer::start().await;
    let events = [
        json!({"type": "message_start", "message": {"id": "msg_1", "model": "claude",
            "content": [], "usage": {"input_tokens": 9, "output_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "Recall "}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "her."}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "signature_delta", "signature": "sig-1"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1,
            "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 1,
            "delta": {"type": "text_delta", "text": "Ada "}}),
        json!({"type": "content_block_delta", "index": 1,
            "delta": {"type": "text_delta", "text": "Lovelace"}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 7}}),
        json!({"type": "message_stop"}),
    ];
    let mut body = String::new();
    for event in events {
        let kind = event["type"].as_str().unwrap_or_default();
        let _ = write!(body, "event: {kind}\ndata: {event}\n\n");
    }
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    let spec = ProviderSpec::new(
        "claude-gateway",
        WireProtocol::AnthropicMessages,
        format!("{}/v1", server.uri()),
    )
    .for_model("claude");
    let harness = Harness::builder()
        .model(Arc::new(ModelClient::from_spec(spec)?))
        .context_budget(unbounded_context_budget())
        .build()?;

    // Act
    let request = RunRequest::new("Who?").output(OutputMode::Text);
    let (stream, handle) = harness.run(request).into_parts();
    let (seen, outcome) = timeout(HANG, async {
        tokio::join!(drain(stream), handle.outcome())
    })
    .await?;

    // Assert
    let output = RunOutput::Text("Ada Lovelace".to_string());
    assert_eq!(outcome.result.as_ref().ok(), Some(&output));
    assert_eq!(
        seen[..6],
        [
            Seen::Started(0, ItemKind::Reasoning),
            Seen::reasoning(0, "Recall "),
            Seen::reasoning(0, "her."),
            Seen::Started(1, ItemKind::Message),
            Seen::text(1, "Ada "),
            Seen::text(1, "Lovelace"),
        ]
    );
    assert!(seen.contains(&Seen::message(1, "Ada Lovelace")));

    Ok(())
}
