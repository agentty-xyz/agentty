use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_contracts::{PermissionMode, ReasoningLevel, SpeedMode};
use ag_protocol::{ProtocolRequestProfile, TurnPrompt};
use ag_session::test_support as model_fixture;
use ag_telemetry::Span;
use mockall::Sequence;
use opentelemetry::{KeyValue, global};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use serde_json::Value;
use tempfile::tempdir;
use tokio::sync::mpsc;

use super::support::remember_request_id;
use crate::agent::app_server::codex::lifecycle::{
    CodexRuntimeState, CodexTurnEventLoopState, compaction_timeout_error, finalize_turn_completion,
    initialize_runtime, start_runtime, start_runtime_with_built_command,
    turn_completed_timeout_error,
};
use crate::agent::app_server::stdio_transport::MockAppServerRuntimeTransport as MockCodexRuntimeTransport;
use crate::app_server::{AppServerError, AppServerTurnRequest};
use crate::telemetry::TRACER_PROVIDER_LOCK;

#[tokio::test]
async fn codex_turn_processing_traces_only_operations_from_the_active_turn_and_thread() {
    // Arrange
    let _guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = InMemorySpanExporter::default();
    global::set_tracer_provider(
        SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build(),
    );
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

    // Act
    Span::root("agent.attempt", Vec::new()).scope(async {
        let mut state = CodexTurnEventLoopState::new(stream_tx, ProtocolRequestProfile::SessionTurn, "thread-1");
        state.process_turn_start_response(&serde_json::json!({"result": {"turn": {"id": "turn-1"}}})).expect("turn start");
        for (method, thread, turn, id) in [
            ("item/started", "thread-1", "turn-1", "call-1"),
            ("item/completed", "thread-2", "turn-1", "call-1"),
            ("item/started", "thread-1", "turn-2", "other-call"),
            ("item/completed", "thread-1", "turn-1", "call-1"),
        ] {
            state.process_stream_response(&serde_json::json!({"method": method, "params": {
                "threadId": thread, "turnId": turn,
                "item": {"id": id, "type": "commandExecution", "exitCode": 0, "command": "cargo test private command", "durationMs": 30},
            }})).expect("stream event");
        }
        state.process_stream_response(&serde_json::json!({"method": "turn/completed", "params": {
            "threadId": "thread-1", "turn": {"id": "turn-1", "status": "completed"},
        }})).expect("turn completion without a snapshot");
    }).await;

    // Assert
    let spans = exporter.get_finished_spans().expect("spans");
    assert_eq!(spans.len(), 2);
    let attempt = spans
        .iter()
        .find(|span| span.name == "agent.attempt")
        .expect("attempt");
    let tool = spans
        .iter()
        .find(|span| span.name == "agent.tool")
        .expect("tool");
    assert_eq!(tool.parent_span_id, attempt.span_context.span_id());
    assert!(
        tool.attributes
            .contains(&KeyValue::new("agentty.timing.source", "lifecycle"))
    );
    assert!(
        tool.attributes
            .contains(&KeyValue::new("agentty.outcome", "completed"))
    );
    assert!(!format!("{spans:?}").contains("private"));
}

#[tokio::test]
async fn codex_terminal_items_complete_responses_and_deduplicate_item_notifications() {
    // Arrange
    let _guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = InMemorySpanExporter::default();
    global::set_tracer_provider(
        SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build(),
    );
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();
    let items = serde_json::json!([
        {"id": "completion-only", "type": "agentMessage", "text": "private final response"},
        {"id": "started", "type": "agentMessage", "text": "private final response"},
        {"id": "already-completed", "type": "agentMessage", "text": "private final response"},
        {"id": "command", "type": "commandExecution", "command": "cargo test private command", "exitCode": 17, "durationMs": 30},
    ]);

    // Act
    Span::root("agent.attempt", Vec::new()).scope(async {
        let mut state = CodexTurnEventLoopState::new(stream_tx, ProtocolRequestProfile::SessionTurn, "thread-1");
        state.process_turn_start_response(&serde_json::json!({"result": {"turn": {"id": "turn-1"}}})).expect("turn start");
        for item in items.as_array().expect("items").iter().skip(1) {
            state.process_stream_response(&serde_json::json!({"method": "item/started", "params": {
                "threadId": "thread-1", "turnId": "turn-1", "item": item,
            }})).expect("item start");
        }
        state.process_stream_response(&serde_json::json!({"method": "item/completed", "params": {
            "threadId": "thread-1", "turnId": "turn-1", "item": items[2],
        }})).expect("standalone completion");
        let result = state.process_stream_response(&serde_json::json!({"method": "turn/completed", "params": {
            "threadId": "thread-1", "turn": {"id": "turn-1", "status": "completed", "items": items},
        }})).expect("turn completion");
        assert_eq!(result, Some(("private final response".to_string(), 0, 0)));
    }).await;

    // Assert
    let spans = exporter.get_finished_spans().expect("spans");
    let attempt = spans
        .iter()
        .find(|span| span.name == "agent.attempt")
        .expect("attempt");
    assert_eq!(spans.len(), 5);
    let responses = spans
        .iter()
        .filter(|span| span.name == "agent.response")
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 3);
    for response in &responses {
        assert_eq!(response.parent_span_id, attempt.span_context.span_id());
        assert_eq!(
            response.span_context.trace_id(),
            attempt.span_context.trace_id()
        );
        assert!(
            response
                .attributes
                .contains(&KeyValue::new("agentty.outcome", "completed"))
        );
    }
    assert_eq!(
        responses
            .iter()
            .filter(|span| span
                .attributes
                .contains(&KeyValue::new("agentty.timing.source", "lifecycle")))
            .count(),
        2
    );
    assert_eq!(
        responses
            .iter()
            .filter(|span| span
                .attributes
                .contains(&KeyValue::new("agentty.timing.source", "completion")))
            .count(),
        1
    );
    let tool = spans
        .iter()
        .find(|span| span.name == "agent.tool")
        .expect("command");
    assert!(
        tool.attributes
            .contains(&KeyValue::new("agentty.outcome", "failed"))
    );
    assert!(
        tool.attributes
            .contains(&KeyValue::new("agentty.provider.exit_code", 17_i64))
    );
    assert!(
        tool.attributes
            .contains(&KeyValue::new("agentty.provider.duration_ms", 30.0))
    );
    assert!(!format!("{spans:?}").contains("private"));
}

#[tokio::test]
async fn codex_terminal_items_require_a_matching_successful_turn_and_thread() {
    // Arrange
    let _guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = InMemorySpanExporter::default();
    global::set_tracer_provider(
        SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build(),
    );
    let ignored_completions = [
        serde_json::json!({"threadId": "thread-1", "turnId": "turn-1", "turn": {"id": "other-turn", "status": "completed"}}),
        serde_json::json!({"threadId": "thread-1", "turn": {"status": "completed"}}),
        serde_json::json!({"threadId": "other-thread", "turn": {"id": "turn-1", "status": "completed"}}),
        serde_json::json!({"threadId": "thread-1", "turn": {"id": "turn-1", "status": "failed"}}),
        serde_json::json!({"threadId": "thread-1", "turn": {"id": "turn-1", "status": "interrupted"}}),
    ];

    // Act
    Span::root("agent.attempt", Vec::new()).scope(async {
        for mut params in ignored_completions.clone() {
            let (stream_tx, _stream_rx) = mpsc::unbounded_channel();
            let mut state = CodexTurnEventLoopState::new(stream_tx, ProtocolRequestProfile::SessionTurn, "thread-1");
            state.process_turn_start_response(&serde_json::json!({"result": {"turn": {"id": "turn-1"}}})).expect("turn start");
            state.process_stream_response(&serde_json::json!({"method": "item/started", "params": {
                "threadId": "thread-1", "turnId": "turn-1", "item": {"id": "started", "type": "agentMessage"},
            }})).expect("item start");
            params["turn"]["items"] = serde_json::json!([
                {"id": "started", "type": "agentMessage", "text": "private ignored response"},
                {"id": "completion-only", "type": "agentMessage", "text": "private ignored response"},
            ]);
            let _ = state.process_stream_response(&serde_json::json!({"method": "turn/completed", "params": params}));
        }
    }).await;

    // Assert
    let spans = exporter.get_finished_spans().expect("spans");
    assert_eq!(spans.len(), ignored_completions.len() + 1);
    for response in spans.iter().filter(|span| span.name == "agent.response") {
        assert!(
            response
                .attributes
                .contains(&KeyValue::new("agentty.timing.source", "lifecycle"))
        );
        assert!(
            response
                .attributes
                .contains(&KeyValue::new("agentty.outcome", "canceled"))
        );
    }
    assert!(!format!("{spans:?}").contains("private"));
}

#[tokio::test]
async fn start_runtime_omits_personality_from_the_process_command() {
    // Arrange
    let runtime_parent = tempdir().expect("create runtime parent");
    let request = AppServerTurnRequest {
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        provider_call_budget: None,
        folder: runtime_parent.path().join("missing-runtime"),
        live_transcript: None,
        main_checkout_root: None,
        model: model_fixture::CODEX_MODEL.as_str().to_string(),
        permission_mode: PermissionMode::AutoEdit,
        personality: ag_contracts::PersonalityPrompt::active("Review carefully.".to_string(), true),
        prompt: TurnPrompt::from("Run the turn"),
        request_kind: ag_contracts::AgentRequestKind::SessionStart,
        replay_transcript: None,
        provider_conversation_id: None,
        persisted_instruction_conversation_id: None,
        reasoning_level: ReasoningLevel::High,
        session_id: "session-1".to_string(),
        speed_mode: SpeedMode::default(),
    };

    // Act
    let result = start_runtime(&request).await;

    // Assert
    assert!(matches!(
        result,
        Err(error)
            if error.to_string().contains("Failed to spawn `codex app-server`")
                && !error.to_string().contains("Review carefully.")
    ));
}

#[tokio::test]
async fn start_runtime_with_built_command_bootstraps_thread_start_with_the_requested_speed() {
    // Arrange
    let folder = tempdir().expect("create runtime folder");
    let request = AppServerTurnRequest {
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        provider_call_budget: None,
        folder: folder.path().to_path_buf(),
        live_transcript: None,
        main_checkout_root: None,
        model: model_fixture::CODEX_MODEL.as_str().to_string(),
        permission_mode: PermissionMode::AutoEdit,
        personality: ag_contracts::PersonalityPrompt::default(),
        prompt: TurnPrompt::from("Run the turn"),
        request_kind: ag_contracts::AgentRequestKind::SessionStart,
        replay_transcript: None,
        provider_conversation_id: None,
        persisted_instruction_conversation_id: None,
        reasoning_level: ReasoningLevel::High,
        session_id: "session-1".to_string(),
        speed_mode: SpeedMode::Fast,
    };

    // Act
    let result =
        start_runtime_with_built_command(std::process::Command::new("cat"), &request).await;

    // Assert
    let error = result
        .err()
        .expect("an echoing runtime should not return a usable thread id");
    assert!(
        error.to_string().contains("thread/start"),
        "unexpected bootstrap error: {error}"
    );
}

#[test]
fn codex_runtime_state_new_initializes_zero_tokens_and_empty_thread_id() {
    // Arrange
    let folder = PathBuf::from("/tmp/agentty-codex-state");
    let model = model_fixture::CODEX_MODEL.as_str().to_string();

    // Act
    let state = CodexRuntimeState::new(folder.clone(), model.clone(), PermissionMode::AutoEdit);

    // Assert
    assert_eq!(state.folder, folder);
    assert_eq!(state.model, model);
    assert_eq!(state.latest_input_tokens, 0);
    assert!(!state.restored_context);
    assert_eq!(state.thread_id, "");
}

#[test]
fn final_completion_fallback_uses_only_the_active_protocol_profile() {
    // Arrange
    let assistant_messages = vec![
        r#"{"project_impact":[],"suggestions":[]}"#.to_string(),
        "later status text".to_string(),
    ];
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

    // Act
    let result = finalize_turn_completion(
        Ok(()),
        None,
        &assistant_messages,
        ProtocolRequestProfile::SessionTurn,
        &stream_tx,
        12,
        34,
    )
    .expect("completed turn should produce a response");

    // Assert
    assert_eq!(result, ("later status text".to_string(), 12, 34));
}

#[test]
fn turn_completed_timeout_error_message_includes_seconds_and_method() {
    // Arrange
    let timeout = Duration::from_secs(123);

    // Act
    let error = turn_completed_timeout_error(timeout);

    // Assert
    let message = error.to_string();
    assert!(message.contains("123"));
    assert!(message.contains("turn/completed"));
}

#[test]
fn compaction_timeout_error_message_includes_seconds_and_compaction_label() {
    // Arrange
    let timeout = Duration::from_secs(456);

    // Act
    let error = compaction_timeout_error(timeout);

    // Assert
    let message = error.to_string();
    assert!(message.contains("456"));
    assert!(message.contains("compaction"));
}

#[tokio::test]
async fn initialize_runtime_writes_initialize_payload_then_initialized_notification() {
    // Arrange
    let request_id = Arc::new(Mutex::new(None));
    let mut transport = MockCodexRuntimeTransport::new();
    let mut sequence = Sequence::new();

    transport
        .expect_write_json_line()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|payload| {
            payload.get("method").and_then(Value::as_str) == Some("initialize")
                && payload.get("id").and_then(Value::as_str).is_some()
        })
        .returning({
            let request_id = Arc::clone(&request_id);

            move |payload| {
                remember_request_id(&request_id, &payload);

                Box::pin(async { Ok(()) })
            }
        });
    transport
        .expect_wait_for_response_line()
        .times(1)
        .in_sequence(&mut sequence)
        .returning({
            let request_id = Arc::clone(&request_id);

            move |_| {
                let response_id = request_id
                    .lock()
                    .expect("initialize id mutex should lock")
                    .clone()
                    .expect("initialize id should be recorded");

                Box::pin(async move {
                    Ok(serde_json::json!({"id": response_id, "result": {}}).to_string())
                })
            }
        });
    transport
        .expect_write_json_line()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|payload| {
            payload.get("method").and_then(Value::as_str) == Some("initialized")
                && payload.get("id").is_none()
        })
        .returning(|_| Box::pin(async { Ok(()) }));

    // Act
    let result = initialize_runtime(&mut transport).await;

    // Assert
    assert!(
        result.is_ok(),
        "initialize_runtime should succeed: {result:?}"
    );
}

#[tokio::test]
async fn initialize_runtime_propagates_transport_termination_error() {
    // Arrange
    let mut transport = MockCodexRuntimeTransport::new();

    transport
        .expect_write_json_line()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    transport
        .expect_wait_for_response_line()
        .times(1)
        .returning(|_| {
            Box::pin(async {
                Err(crate::app_server_transport::AppServerTransportError::ProcessTerminated)
            })
        });

    // Act
    let result = initialize_runtime(&mut transport).await;

    // Assert
    let error = result.expect_err("initialize_runtime should propagate transport error");
    assert!(matches!(error, AppServerError::Transport(_)));
}
