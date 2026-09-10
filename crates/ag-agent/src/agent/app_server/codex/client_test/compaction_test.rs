use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_protocol::ProtocolRequestProfile;
use mockall::Sequence;
use serde_json::Value;
use tokio::sync::mpsc;

use super::support::{
    build_runtime_state, expect_context_overflow_turn, expect_proactive_compaction_turn,
    expect_reactive_compaction, expect_retried_turn_after_compaction, remember_request_id,
};
use crate::agent::app_server::codex::{lifecycle, policy};
use crate::agent::app_server::stdio_transport::MockAppServerRuntimeTransport as MockCodexRuntimeTransport;
use crate::app_server::AppServerStreamEvent;
use crate::model::agent::{AgentModel, ReasoningLevel};
use crate::model::session::SpeedMode;

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
    let gpt_6_astra_model = AgentModel::Gpt6Astra.as_str();
    let gpt_56_sol_model = AgentModel::Gpt56Sol.as_str();
    let gpt_56_terra_model = AgentModel::Gpt56Terra.as_str();
    let gpt_56_luna_model = AgentModel::Gpt56Luna.as_str();
    let gpt_55_model = AgentModel::Gpt56Sol.as_str();
    let spark_model = AgentModel::Gpt53CodexSpark.as_str();

    // Act
    let gpt_6_astra_threshold = policy::auto_compact_input_token_threshold(gpt_6_astra_model);
    let gpt_56_sol_threshold = policy::auto_compact_input_token_threshold(gpt_56_sol_model);
    let gpt_56_terra_threshold = policy::auto_compact_input_token_threshold(gpt_56_terra_model);
    let gpt_56_luna_threshold = policy::auto_compact_input_token_threshold(gpt_56_luna_model);
    let gpt_55_threshold = policy::auto_compact_input_token_threshold(gpt_55_model);
    let spark_threshold = policy::auto_compact_input_token_threshold(spark_model);

    // Assert
    assert_eq!(
        gpt_6_astra_threshold,
        policy::AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_1050K_CONTEXT
    );
    assert_eq!(
        gpt_56_sol_threshold,
        policy::AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_1050K_CONTEXT
    );
    assert_eq!(
        gpt_56_terra_threshold,
        policy::AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_1050K_CONTEXT
    );
    assert_eq!(
        gpt_56_luna_threshold,
        policy::AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_1050K_CONTEXT
    );
    assert_eq!(
        gpt_55_threshold,
        policy::AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_1050K_CONTEXT
    );
    assert_eq!(
        spark_threshold,
        policy::AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_128K_CONTEXT
    );
}

#[tokio::test]
async fn send_compact_request_resets_latest_input_tokens_on_success() {
    // Arrange
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
    let result =
        lifecycle::send_compact_request(&mut transport, "thread-1", &mut latest_input_tokens).await;

    // Assert
    result.expect("compaction should succeed");
    assert_eq!(latest_input_tokens, 0);
}

#[tokio::test]
async fn run_turn_with_runtime_compacts_proactively_before_turn_start() {
    // Arrange
    let mut state = build_runtime_state(
        "thread-1",
        policy::AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_1050K_CONTEXT,
    );
    let compact_id = Arc::new(Mutex::new(None));
    let turn_id = Arc::new(Mutex::new(None));
    let mut transport = MockCodexRuntimeTransport::new();
    let mut sequence = Sequence::new();
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

    expect_proactive_compaction_turn(&mut transport, &mut sequence, compact_id, turn_id);

    // Act
    let result = lifecycle::run_turn_with_runtime(
        &mut transport,
        &mut state,
        "Implement the task",
        ProtocolRequestProfile::SessionTurn,
        ReasoningLevel::default(),
        SpeedMode::default(),
        stream_tx,
    )
    .await;

    // Assert
    let (message, input_tokens, output_tokens) =
        result.expect("turn should complete after proactive compaction");
    assert_eq!(message, String::new());
    assert_eq!(input_tokens, 12);
    assert_eq!(output_tokens, 3);
    assert_eq!(state.latest_input_tokens, 12);
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
    let result = lifecycle::run_turn_with_runtime(
        &mut transport,
        &mut state,
        "Implement the task",
        ProtocolRequestProfile::SessionTurn,
        ReasoningLevel::default(),
        SpeedMode::Fast,
        stream_tx,
    )
    .await;

    // Assert
    let (message, input_tokens, output_tokens) =
        result.expect("turn should complete after reactive compaction");
    assert_eq!(message, String::new());
    assert_eq!(input_tokens, 21);
    assert_eq!(output_tokens, 5);
    assert_eq!(state.latest_input_tokens, 21);
    assert_eq!(
        stream_rx.try_recv().ok(),
        Some(AppServerStreamEvent::ProgressUpdate(
            "Compacting context".to_string()
        ))
    );
}
