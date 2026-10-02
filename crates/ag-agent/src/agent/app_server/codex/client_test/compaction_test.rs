use std::future::{self, Future};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use ag_contracts::{ReasoningLevel, SpeedMode};
use ag_protocol::ProtocolRequestProfile;
use ag_session::AgentModel;
use ag_telemetry::Span;
use mockall::Sequence;
use opentelemetry::{KeyValue, global};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use serde_json::Value;
use tokio::sync::mpsc;

use super::support::{
    build_runtime_state, expect_context_overflow_turn, expect_proactive_compaction_turn,
    expect_reactive_compaction, expect_retried_turn_after_compaction, remember_request_id,
};
use crate::agent::app_server::codex::{lifecycle, policy};
use crate::agent::app_server::stdio_transport::MockAppServerRuntimeTransport as MockCodexRuntimeTransport;
use crate::app_server::AppServerStreamEvent;
use crate::app_server_transport::AppServerTransportError;
use crate::telemetry::TRACER_PROVIDER_LOCK;

#[test]
fn compaction_timeout_error_includes_timeout_seconds() {
    // Arrange
    let timeout = Duration::from_mins(70);

    // Act
    let error = lifecycle::compaction_timeout_error(timeout);

    // Assert
    let error_message = error.to_string();
    assert!(error_message.contains("4200"));
    assert!(error_message.contains("compaction"));
}

#[test]
fn auto_compact_input_token_threshold_uses_1050k_limit_for_codex_models() {
    // Arrange
    let large_context_models = [
        AgentModel::Gpt6Astra,
        AgentModel::Gpt61Sol,
        AgentModel::Gpt6Luna,
        AgentModel::Gpt56Terra,
    ];
    let spark_model = AgentModel::Gpt53CodexSpark.as_str();

    // Act
    let large_context_thresholds = large_context_models
        .map(|model| policy::auto_compact_input_token_threshold(model.as_str()));
    let spark_threshold = policy::auto_compact_input_token_threshold(spark_model);

    // Assert
    assert_eq!(large_context_thresholds, [922_000; 4]);
    assert_eq!(spark_threshold, 120_000);
}

#[test]
fn undeclared_and_foreign_models_use_conservative_compaction_budget() {
    // Arrange
    let models = [
        "unknown-model",
        "claude-opus-5-5",
        "gemini-3.1-pro-preview",
        "gpt-6-sol",
    ];

    // Act
    let budgets = models.map(policy::auto_compact_input_token_threshold);

    // Assert
    assert_eq!(budgets, [120_000; 4]);
}

#[tokio::test]
async fn send_compact_request_resets_latest_input_tokens_on_success() {
    // Arrange
    let _guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = install_provider();
    let compact_id = Arc::new(Mutex::new(None));
    let mut latest_input_tokens = 450_000;
    let mut transport = MockCodexRuntimeTransport::new();
    let mut sequence = Sequence::new();

    transport
        .expect_write_json_line()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|payload| {
            payload.get("method").and_then(Value::as_str) == Some("thread/compact/start")
        })
        .returning({
            let compact_id = Arc::clone(&compact_id);

            move |payload| {
                remember_request_id(&compact_id, &payload);

                Box::pin(async { Ok(()) })
            }
        });
    transport
        .expect_wait_for_response_line()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(move |_| {
            let response_id = compact_id
                .lock()
                .expect("compact mutex should lock")
                .clone()
                .expect("compact id should be recorded");

            Box::pin(
                async move { Ok(serde_json::json!({"id": response_id, "result": {}}).to_string()) },
            )
        });
    for line in [
        "\n",
        "private malformed notification",
        r#"{"method":"item/started","params":{"item":{"type":"contextCompaction","id":"private-item"}}}"#,
    ] {
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move || Box::pin(async move { Ok(Some(line.to_string())) }));
    }
    transport
        .expect_next_stdout()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|| {
            Box::pin(async {
                Ok(Some(
                    serde_json::json!({
                        "method": "turn/completed",
                        "params": {"turn": {"status": "completed"}}
                    })
                    .to_string(),
                ))
            })
        });

    // Act
    let result = Span::root("agent.attempt", Vec::new())
        .scope(lifecycle::send_compact_request(
            &mut transport,
            "thread-1",
            &mut latest_input_tokens,
        ))
        .await;

    // Assert
    result.expect("compaction should succeed");
    assert_eq!(latest_input_tokens, 0);
    assert_compaction_span(&exporter, "completed");
}

#[tokio::test]
async fn run_turn_with_runtime_compacts_proactively_before_turn_start() {
    // Arrange
    let _guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = install_provider();
    let mut state = build_runtime_state("thread-1", 922_000);
    let compact_id = Arc::new(Mutex::new(None));
    let turn_id = Arc::new(Mutex::new(None));
    let mut transport = MockCodexRuntimeTransport::new();
    let mut sequence = Sequence::new();
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

    expect_proactive_compaction_turn(&mut transport, &mut sequence, compact_id, turn_id);

    // Act
    let result = Span::root("agent.attempt", Vec::new())
        .scope(lifecycle::run_turn_with_runtime(
            &mut transport,
            &mut state,
            "Implement the task",
            ProtocolRequestProfile::SessionTurn,
            ReasoningLevel::default(),
            SpeedMode::default(),
            stream_tx,
        ))
        .await;

    // Assert
    let (message, input_tokens, output_tokens) =
        result.expect("turn should complete after proactive compaction");
    assert_eq!(message, String::new());
    assert_eq!(input_tokens, 12);
    assert_eq!(output_tokens, 3);
    assert_eq!(state.latest_input_tokens, 12);
    assert_compaction_span(&exporter, "completed");
    assert_eq!(
        stream_rx.try_recv().ok(),
        Some(AppServerStreamEvent::ProgressUpdate(
            "Compacting context".to_string()
        ))
    );
}

#[tokio::test]
async fn run_turn_with_runtime_compacts_and_retries_after_context_overflow() {
    // Arrange
    let _guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = install_provider();
    let mut state = build_runtime_state("thread-2", 0);
    let failing_turn_id = Arc::new(Mutex::new(None));
    let compact_id = Arc::new(Mutex::new(None));
    let retried_turn_id = Arc::new(Mutex::new(None));
    let mut transport = MockCodexRuntimeTransport::new();
    let mut sequence = Sequence::new();
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

    expect_context_overflow_turn(&mut transport, &mut sequence, failing_turn_id);
    expect_reactive_compaction(&mut transport, &mut sequence, compact_id);
    expect_retried_turn_after_compaction(&mut transport, &mut sequence, retried_turn_id);

    // Act
    let result = Span::root("agent.attempt", Vec::new())
        .scope(lifecycle::run_turn_with_runtime(
            &mut transport,
            &mut state,
            "Implement the task",
            ProtocolRequestProfile::SessionTurn,
            ReasoningLevel::default(),
            SpeedMode::Fast,
            stream_tx,
        ))
        .await;

    // Assert
    let (message, input_tokens, output_tokens) =
        result.expect("turn should complete after reactive compaction");
    assert_eq!(message, String::new());
    assert_eq!(input_tokens, 21);
    assert_eq!(output_tokens, 5);
    assert_eq!(state.latest_input_tokens, 21);
    assert_compaction_span(&exporter, "completed");
    assert_eq!(
        stream_rx.try_recv().ok(),
        Some(AppServerStreamEvent::ProgressUpdate(
            "Compacting context".to_string()
        ))
    );
}

#[tokio::test]
async fn send_compact_request_traces_request_acknowledgment_and_completion_failures() {
    // Arrange
    let _guard = TRACER_PROVIDER_LOCK.lock().await;
    for stage in [
        "request",
        "acknowledgment",
        "completion",
        "completion_without_error",
    ] {
        let exporter = install_provider();
        let mut latest_input_tokens = 450_000;
        let mut transport = MockCodexRuntimeTransport::new();
        transport
            .expect_write_json_line()
            .times(1)
            .returning(move |_| {
                Box::pin(async move {
                    if stage == "request" {
                        Err(AppServerTransportError::ProcessTerminated)
                    } else {
                        Ok(())
                    }
                })
            });
        if stage != "request" {
            transport
                .expect_wait_for_response_line()
                .times(1)
                .returning(move |_| {
                    Box::pin(async move {
                        if stage == "acknowledgment" {
                            Err(AppServerTransportError::ProcessTerminated)
                        } else {
                            Ok("{}".to_string())
                        }
                    })
                });
        }
        if matches!(stage, "completion" | "completion_without_error") {
            transport.expect_next_stdout().times(1).returning(move || {
                Box::pin(async move {
                    Ok(Some(
                        serde_json::json!({"method": "turn/completed", "params": {"turn": {
                            "status": "failed", "error": if stage == "completion" {
                                serde_json::json!({"message": "private provider failure"})
                            } else { Value::Null },
                        }}})
                        .to_string(),
                    ))
                })
            });
        }

        // Act
        let result = Span::root("agent.attempt", Vec::new())
            .scope(lifecycle::send_compact_request(
                &mut transport,
                "private-thread",
                &mut latest_input_tokens,
            ))
            .await;

        // Assert
        assert!(result.is_err(), "{stage} failure should propagate");
        assert_eq!(latest_input_tokens, 450_000);
        assert_compaction_span(&exporter, "failed");
    }
}

#[tokio::test]
async fn send_compact_request_traces_completion_timeout_as_failed() {
    // Arrange
    let _guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = install_provider();
    let mut latest_input_tokens = 450_000;
    let mut transport = MockCodexRuntimeTransport::new();
    transport
        .expect_write_json_line()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    transport
        .expect_wait_for_response_line()
        .times(1)
        .returning(|_| Box::pin(async { Ok("{}".to_string()) }));
    transport
        .expect_next_stdout()
        .times(1)
        .returning(|| Box::pin(future::pending()));

    // Act
    let result = Span::root("agent.attempt", Vec::new())
        .scope(lifecycle::send_compact_request_with_timeout(
            &mut transport,
            "private-thread",
            &mut latest_input_tokens,
            Duration::from_millis(1),
        ))
        .await;

    // Assert
    assert!(
        result
            .expect_err("completion should time out")
            .to_string()
            .contains("Timed out")
    );
    assert_eq!(latest_input_tokens, 450_000);
    assert_compaction_span(&exporter, "failed");
}

#[tokio::test]
async fn dropping_compaction_while_request_acknowledgment_or_completion_waits_closes_its_span() {
    // Arrange
    let _guard = TRACER_PROVIDER_LOCK.lock().await;
    for stage in ["request", "acknowledgment", "completion"] {
        let exporter = install_provider();
        let mut latest_input_tokens = 450_000;
        let mut transport = MockCodexRuntimeTransport::new();
        transport
            .expect_write_json_line()
            .times(1)
            .returning(move |_| {
                Box::pin(async move {
                    if stage == "request" {
                        future::pending().await
                    } else {
                        Ok(())
                    }
                })
            });
        if stage != "request" {
            transport
                .expect_wait_for_response_line()
                .times(1)
                .returning(move |_| {
                    Box::pin(async move {
                        if stage == "acknowledgment" {
                            future::pending().await
                        } else {
                            Ok("{}".to_string())
                        }
                    })
                });
        }
        if stage == "completion" {
            transport
                .expect_next_stdout()
                .times(1)
                .returning(|| Box::pin(future::pending()));
        }
        let mut request = Box::pin(Span::root("agent.attempt", Vec::new()).scope(
            lifecycle::send_compact_request(
                &mut transport,
                "private-thread",
                &mut latest_input_tokens,
            ),
        ));

        // Act
        future::poll_fn(|context| {
            assert!(Future::poll(request.as_mut(), context).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(request);

        // Assert
        assert_eq!(latest_input_tokens, 450_000);
        assert_compaction_span(&exporter, "canceled");
    }
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

fn assert_compaction_span(exporter: &InMemorySpanExporter, outcome: &'static str) {
    let spans = exporter.get_finished_spans().expect("spans");
    assert_eq!(spans.len(), 2);
    let attempt = spans
        .iter()
        .find(|span| span.name == "agent.attempt")
        .expect("attempt");
    let compaction = spans
        .iter()
        .find(|span| span.name == "agent.compaction")
        .expect("compaction");
    assert_eq!(compaction.parent_span_id, attempt.span_context.span_id());
    assert_eq!(
        compaction.span_context.trace_id(),
        attempt.span_context.trace_id()
    );
    assert!(compaction.start_time >= attempt.start_time);
    assert!(compaction.end_time >= compaction.start_time);
    assert!(compaction.attributes.contains(&KeyValue::new(
        "agentty.provider.operation.type",
        "compaction"
    )));
    assert!(
        compaction
            .attributes
            .contains(&KeyValue::new("agentty.timing.source", "lifecycle"))
    );
    assert!(
        compaction
            .attributes
            .contains(&KeyValue::new("agentty.outcome", outcome))
    );
    assert!(!format!("{spans:?}").contains("private"));
}
