use std::collections::HashSet;
use std::future;
use std::time::{Duration, SystemTime};

use ag_session::AgentKind;
use ag_telemetry::{Context, Outcome, Span};
use opentelemetry::{KeyValue, global};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::agent::trace::{MAX_OPERATIONS, OperationTrace};
use crate::telemetry::TRACER_PROVIDER_LOCK;

#[tokio::test]
async fn codex_operations_keep_attempt_parentage_outcomes_and_numeric_metadata() {
    // Arrange
    let types = [
        "commandExecution",
        "command_execution",
        "fileChange",
        "file_change",
        "mcpToolCall",
        "mcp_tool_call",
        "dynamicToolCall",
        "dynamic_tool_call",
        "webSearch",
        "web_search",
        "reasoning",
        "agentMessage",
        "agent_message",
        "contextCompaction",
        "context_compaction",
        "collabAgentToolCall",
        "collab_agent_tool_call",
        "imageGeneration",
        "image_generation",
    ];
    let mut events = Vec::new();
    for (index, item_type) in types.into_iter().enumerate() {
        let item = json!({"id": index.to_string(), "type": item_type, "command": "private command", "arguments": "private arguments", "text": "private text"});
        let started = json!({"method": "item/started", "params": {"item": item}});
        events.push(started.clone());
        events.push(started);
        let completed = json!({"method": "item/completed", "params": {"item": {
            "id": index.to_string(), "type": item_type, "exitCode": 0, "durationMs": 25,
            "aggregatedOutput": "private output", "error": null,
        }}});
        events.push(completed.clone());
        events.push(completed);
    }
    for (index, extra) in [
        json!({"status": "failed"}),
        json!({"status": "declined"}),
        json!({"status": "denied"}),
        json!({"success": false}),
        json!({"error": {"message": "private error"}}),
        json!({"exitCode": 7}),
        json!({"status": "canceled"}),
        json!({"status": "cancelled"}),
        json!({"status": "interrupted"}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut item = json!({"id": format!("terminal-{index}"), "type": "mcpToolCall"});
        item.as_object_mut()
            .expect("item")
            .extend(extra.as_object().expect("extra").clone());
        events.push(json!({"method": "item/completed", "params": {"item": item}}));
    }

    // Act
    let spans = capture(AgentKind::Codex, events).await;

    // Assert
    assert_children(&spans, types.len() + 9);
    assert_eq!(
        spans
            .iter()
            .filter(|span| span
                .attributes
                .contains(&KeyValue::new("agentty.timing.source", "lifecycle")))
            .count(),
        types.len()
    );
    for name in [
        "agent.tool",
        "agent.response",
        "agent.reasoning",
        "agent.compaction",
        "agent.subagent",
    ] {
        assert!(spans.iter().any(|span| span.name == name));
    }
    let outcomes = outcomes(&spans);
    assert_eq!(
        outcomes.iter().filter(|value| **value == "failed").count(),
        6
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|value| **value == "canceled")
            .count(),
        3
    );
    assert!(spans.iter().any(|span| {
        span.attributes
            .contains(&KeyValue::new("agentty.provider.duration_ms", 25.0))
    }));
    assert!(!format!("{spans:?}").contains("private"));
}

#[tokio::test]
async fn gemini_updates_retain_original_category_and_finish_partial_payloads() {
    // Arrange
    let mut events = Vec::new();
    for (index, kind) in [
        "read",
        "edit",
        "execute",
        "search",
        "fetch",
        "private custom kind",
    ]
    .into_iter()
    .enumerate()
    {
        events.push(gemini(&json!({"sessionUpdate": "tool_call", "toolCallId": index.to_string(), "kind": kind, "status": "pending", "title": "private title", "rawInput": "private input"})));
        events.push(gemini(&json!({"sessionUpdate": "tool_call_update", "toolCallId": index.to_string(), "status": "in_progress"})));
        events.push(gemini(&json!({"sessionUpdate": "tool_call_update", "toolCallId": index.to_string(), "status": "completed", "content": "private output"})));
    }
    for status in ["failed", "canceled", "cancelled"] {
        events.push(gemini(
            &json!({"sessionUpdate": "tool_call_update", "toolCallId": status, "status": status}),
        ));
    }

    // Act
    let spans = capture(AgentKind::Gemini, events).await;

    // Assert
    assert_children(&spans, 9);
    for category in ["read", "file_change", "command", "search", "fetch", "tool"] {
        assert!(spans.iter().any(|span| {
            span.attributes
                .contains(&KeyValue::new("agentty.provider.operation.type", category))
        }));
    }
    assert!(!format!("{spans:?}").contains("private"));
}

#[tokio::test]
async fn antigravity_steps_preserve_reported_durations_and_usage_without_content() {
    // Arrange
    let mut events = Vec::new();
    for (index, kind) in [
        "tool",
        "agent_response",
        "reasoning",
        "thought",
        "context_compaction",
        "context-compaction",
        "context_compression",
        "checkpoint",
    ]
    .into_iter()
    .enumerate()
    {
        events.push(antigravity(&json!({"step_index": index, "step_type": kind, "state": "ACTIVE", "tool_name": "private tool"})));
        events.push(antigravity(&json!({"step_index": index, "step_type": kind, "state": "DONE", "duration_seconds": 0.25,
            "usage": {"input_tokens": 20, "output_tokens": 3, "cache_read_tokens": 4, "thinking_tokens": 2},
            "tool_info": {"output": "private output", "parameters": "private parameters"}, "text_delta": "private text"
        })));
    }
    events.push(antigravity(&json!({"step_index": 10, "step_type": "tool", "state": "done", "tool_info": {"error": {"message": "private error"}}})));
    events.push(antigravity(&json!({"step_index": 11, "step_type": "agent_response", "state": "done", "duration_seconds": 0.1})));
    events.push(antigravity(&json!({"step_index": 12, "step_type": "tool", "state": "done", "subagent_info": {"prompt": "private prompt"}, "usage": {"input_tokens": -1, "output_tokens": u64::MAX, "cache_read_tokens": "private tokens"}})));

    // Act
    let spans = capture(AgentKind::Antigravity, events).await;

    // Assert
    assert_children(&spans, 11);
    assert!(spans.iter().any(|span| span.name == "agent.checkpoint"));
    let subagent = spans
        .iter()
        .find(|span| span.name == "agent.subagent")
        .expect("subagent");
    assert!(
        !subagent
            .attributes
            .iter()
            .any(|attribute| attribute.key.as_str().starts_with("gen_ai.usage"))
    );
    assert!(spans.iter().any(|span| {
        span.attributes
            .contains(&KeyValue::new("agentty.provider.duration_ms", 250.0))
    }));
    for (key, count) in [
        ("gen_ai.usage.input_tokens", 20),
        ("gen_ai.usage.output_tokens", 3),
        ("gen_ai.usage.cache_read.input_tokens", 4),
        ("gen_ai.usage.reasoning.output_tokens", 2),
    ] {
        assert!(
            spans
                .iter()
                .any(|span| span.attributes.contains(&KeyValue::new(key, count)))
        );
    }
    assert!(!format!("{spans:?}").contains("private"));
}

#[tokio::test]
async fn claude_tool_messages_pair_calls_with_results_without_tool_names_or_arguments() {
    // Arrange
    let mut events = Vec::new();
    for (index, name) in [
        "Bash",
        "Read",
        "Edit",
        "Write",
        "MultiEdit",
        "Grep",
        "Glob",
        "WebFetch",
        "WebSearch",
        "Agent",
        "Task",
        "private custom tool",
    ]
    .into_iter()
    .enumerate()
    {
        events.push(json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": index.to_string(), "name": name, "input": "private input"}]}}));
        events.push(json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": index.to_string(), "is_error": index == 0, "content": "private output"}]}}));
    }

    // Act
    let spans = capture(AgentKind::Claude, events).await;

    // Assert
    assert_children(&spans, 12);
    assert!(outcomes(&spans).contains(&"failed"));
    assert!(!format!("{spans:?}").contains("private"));
}

#[tokio::test]
async fn claude_direct_and_wrapped_tool_messages_pair_across_formats() {
    // Arrange
    let mut events = Vec::new();
    for (index, (direct_call, direct_result)) in
        [(true, true), (true, false), (false, true), (false, false)]
            .into_iter()
            .enumerate()
    {
        let call = json!({"type": "message", "role": "assistant", "content": [
            {"type": "text", "text": "private explanation"},
            {"type": "tool_use", "id": index.to_string(), "name": "Bash", "input": {"command": "private command"}},
        ]});
        let result = json!({"type": "message", "role": "user", "content": [
            {"type": "tool_result", "tool_use_id": index.to_string(), "is_error": index == 0, "content": "private output"},
        ]});
        events.push(if direct_call {
            call
        } else {
            json!({"type": "assistant", "message": call})
        });
        let completed = if direct_result {
            result
        } else {
            json!({"type": "user", "message": result})
        };
        events.push(completed.clone());
        events.push(completed);
    }

    // Act
    let spans = capture(AgentKind::Claude, events).await;

    // Assert
    assert_children(&spans, 4);
    assert!(!format!("{spans:?}").contains("private"));
    let tools = spans
        .into_iter()
        .filter(|span| span.name == "agent.tool")
        .collect::<Vec<_>>();
    assert_eq!(tools.len(), 4);
    for span in &tools {
        assert!(
            span.attributes
                .contains(&KeyValue::new("agentty.provider.operation.type", "command"))
        );
        assert!(
            span.attributes
                .contains(&KeyValue::new("agentty.timing.source", "lifecycle"))
        );
    }
    let outcomes = outcomes(&tools);
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == "completed")
            .count(),
        3
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == "failed")
            .count(),
        1
    );
}

#[tokio::test]
async fn malformed_unknown_and_unrelated_provider_events_are_ignored() {
    // Arrange
    let fixtures = [
        (
            AgentKind::Codex,
            vec![
                json!({}),
                json!({"method": "other"}),
                json!({"method": "item/started"}),
                json!({"method": "item/started", "params": {"item": {}}}),
                json!({"method": "item/started", "params": {"item": {"id": "one", "type": "unknown"}}}),
            ],
        ),
        (
            AgentKind::Gemini,
            vec![
                json!({}),
                json!({"method": "session/update"}),
                gemini(&json!({"sessionUpdate": "agent_message_chunk"})),
                gemini(&json!({"sessionUpdate": "tool_call"})),
            ],
        ),
        (
            AgentKind::Antigravity,
            vec![
                json!({}),
                json!({"event": "step_update"}),
                antigravity(&json!({})),
                antigravity(&json!({"step_index": 0, "step_type": "user_input"})),
                antigravity(&json!({"step_index": 0, "step_type": "tool", "state": "unknown"})),
            ],
        ),
        (
            AgentKind::Claude,
            vec![
                json!({}),
                json!({"message": {}}),
                json!({"type": "message", "content": "private non-array content"}),
                json!({"type": "message", "content": [{"type": "text"}, {"type": "tool_use"}, {"type": "tool_result"}]}),
                json!({"type": "result", "content": [{"type": "tool_use", "id": "unrelated", "name": "Bash"}]}),
                json!({"message": {"content": [{"type": "text"}, {"type": "tool_use"}, {"type": "tool_result"}]}}),
            ],
        ),
    ];

    // Act / Assert
    for (kind, events) in fixtures {
        assert_children(&capture(kind, events).await, 0);
    }
}

#[tokio::test]
async fn pending_operations_close_when_the_attempt_future_is_canceled() {
    // Arrange
    let _guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = install_provider();
    let (ready_tx, ready_rx) = oneshot::channel();
    let root = Span::root("agent.attempt", Vec::new());

    // Act
    let task = tokio::spawn(root.scope(async move {
        let mut trace = OperationTrace::new();
        trace.observe(AgentKind::Gemini, &gemini(&json!({"sessionUpdate": "tool_call", "toolCallId": "unfinished", "kind": "execute"})));
        let _ = ready_tx.send(());
        future::pending::<()>().await;
    }));
    ready_rx.await.expect("operation started");
    task.abort();
    let _ = task.await;

    // Assert
    let spans = exporter.get_finished_spans().expect("spans");
    assert_children(&spans, 1);
    assert!(
        outcomes(&spans)
            .iter()
            .all(|outcome| *outcome == "canceled")
    );
}

#[tokio::test]
async fn reconstructed_and_observed_intervals_are_distinguished_and_bounded() {
    // Arrange
    let _guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = install_provider();
    let started_at = SystemTime::now() - Duration::from_secs(1);
    let root = Span::child_at("agent.attempt", started_at);

    // Act
    root.scope(async {
        let mut trace = OperationTrace::new();
        trace.started_at = started_at;
        trace.record(
            "reported",
            "command",
            Some(Outcome::Completed),
            Some(125.0),
            Vec::new(),
        );
        trace.record(
            "clamped",
            "tool",
            Some(Outcome::Completed),
            Some(2000.0),
            Vec::new(),
        );
        trace.record(
            "negative",
            "tool",
            Some(Outcome::Completed),
            Some(-1.0),
            Vec::new(),
        );
        trace.record(
            "nan",
            "tool",
            Some(Outcome::Completed),
            Some(f64::NAN),
            Vec::new(),
        );
        trace.record(
            "overflow",
            "tool",
            Some(Outcome::Completed),
            Some(f64::MAX),
            Vec::new(),
        );
        trace.record("", "tool", None, None, Vec::new());
        trace.record(&"x".repeat(257), "tool", None, None, Vec::new());
        trace.observe_line(AgentKind::Claude, b"invalid json");
    })
    .await;

    // Assert
    let spans = exporter.get_finished_spans().expect("spans");
    assert_children(&spans, 5);
    let reported = spans
        .iter()
        .find(|span| {
            span.attributes
                .contains(&KeyValue::new("agentty.provider.duration_ms", 125.0))
        })
        .expect("reconstructed interval");
    assert!(
        reported
            .attributes
            .contains(&KeyValue::new("agentty.timing.source", "provider"))
    );
    assert!(
        reported
            .end_time
            .duration_since(reported.start_time)
            .expect("ordered times")
            >= Duration::from_millis(125)
    );
    let clamped = spans
        .iter()
        .find(|span| {
            span.attributes
                .contains(&KeyValue::new("agentty.provider.duration_ms", 2000.0))
        })
        .expect("clamped interval");
    assert_eq!(clamped.start_time, started_at);
    for span in &spans {
        assert!(span.start_time >= started_at);
    }
    assert_eq!(
        spans
            .iter()
            .filter(|span| span
                .attributes
                .contains(&KeyValue::new("agentty.timing.source", "completion")))
            .count(),
        3
    );
}

#[tokio::test]
async fn capacity_limits_new_operations_but_allows_existing_operations_to_finish() {
    // Arrange
    let _guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = install_provider();
    let root = Span::root("agent.attempt", Vec::new());

    // Act
    root.scope(async {
        let mut trace = OperationTrace::new();
        trace.record("active", "tool", None, None, Vec::new());
        trace.completed = (0..MAX_OPERATIONS - 1)
            .map(|index| index.to_string())
            .collect::<HashSet<_>>();
        trace.record("rejected", "tool", None, None, Vec::new());
        trace.record("active", "tool", Some(Outcome::Completed), None, Vec::new());
        assert_eq!(trace.completed.len(), MAX_OPERATIONS);
        assert!(trace.operations.is_empty());
    })
    .await;

    // Assert
    assert_children(&exporter.get_finished_spans().expect("spans"), 1);
}

#[test]
fn disabled_tracing_ignores_provider_payloads() {
    // Arrange
    let _guard = Context::new().attach();
    let mut trace = OperationTrace::new();

    // Act
    trace.observe(AgentKind::Codex, &json!({"method": "item/started", "params": {"item": {"id": "one", "type": "commandExecution"}}}));
    trace.observe_line(AgentKind::Claude, b"{}");
    trace.observe_codex_completed_turn(
        &json!({"params": {"turn": {"items": [{"id": "one", "type": "commandExecution"}]}}}),
    );

    // Assert
    assert!(trace.operations.is_empty());
}

async fn capture(kind: AgentKind, events: Vec<Value>) -> Vec<SpanData> {
    let _guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = install_provider();
    Span::root("agent.attempt", Vec::new())
        .scope(async {
            let mut trace = OperationTrace::new();
            for event in events {
                trace.observe_line(kind, &serde_json::to_vec(&event).expect("event"));
            }
        })
        .await;

    exporter.get_finished_spans().expect("spans")
}

fn install_provider() -> InMemorySpanExporter {
    let exporter = InMemorySpanExporter::default();
    global::set_tracer_provider(
        SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build(),
    );

    exporter
}

fn assert_children(spans: &[SpanData], count: usize) {
    let root = spans
        .iter()
        .find(|span| span.name == "agent.attempt")
        .expect("attempt");
    assert_eq!(spans.len(), count + 1);
    for child in spans.iter().filter(|span| span.name != "agent.attempt") {
        assert_eq!(child.parent_span_id, root.span_context.span_id());
        assert_eq!(child.span_context.trace_id(), root.span_context.trace_id());
    }
}

fn outcomes(spans: &[SpanData]) -> Vec<&str> {
    spans
        .iter()
        .filter_map(|span| {
            span.attributes
                .iter()
                .find(|attribute| attribute.key.as_str() == "agentty.outcome")
        })
        .filter_map(|attribute| match &attribute.value {
            opentelemetry::Value::String(value) => Some(value.as_str()),
            _ => None,
        })
        .collect()
}

fn gemini(update: &Value) -> Value {
    json!({"method": "session/update", "params": {"sessionId": "session", "update": update}})
}

fn antigravity(step: &Value) -> Value {
    json!({"event": "step_update", "step_update": step})
}
