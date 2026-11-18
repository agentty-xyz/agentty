//! Codex `turn/steer` delivery tests for the turn event loop.

use std::time::Duration;

use ag_contracts::{PermissionMode, ReasoningLevel, SpeedMode};
use ag_protocol::{ProtocolRequestProfile, TurnPrompt};
use ag_session::test_support as model_fixture;
use serde_json::Value;
use tempfile::tempdir;
use tokio::sync::mpsc;

use crate::agent::app_server::codex::lifecycle::{
    CodexRuntimeState, CodexTurnEventLoopInput, CodexTurnOptions, build_turn_steer_payload,
    execute_turn_event_loop, run_turn_with_runtime,
};
use crate::agent::app_server::codex::usage;
use crate::agent::app_server::stdio_transport::{
    AppServerRuntimeTransport, AppServerTransportFuture,
};
use crate::app_server::{AppServerError, AppServerStreamEvent, TurnSteeringInbox};
use crate::app_server_transport::AppServerTransportError;

/// Channel-backed transport whose stdout reads are cancel-safe.
struct ScriptedTransport {
    stdout: mpsc::UnboundedReceiver<String>,
    writes: mpsc::UnboundedSender<Value>,
}

impl AppServerRuntimeTransport for ScriptedTransport {
    fn write_json_line(
        &mut self,
        payload: Value,
    ) -> AppServerTransportFuture<'_, Result<(), AppServerTransportError>> {
        let _ = self.writes.send(payload);

        Box::pin(async { Ok(()) })
    }

    fn wait_for_response_line(
        &mut self,
        _response_id: String,
    ) -> AppServerTransportFuture<'_, Result<String, AppServerTransportError>> {
        Box::pin(async { Ok(String::new()) })
    }

    fn next_stdout(
        &mut self,
    ) -> AppServerTransportFuture<'_, Result<Option<String>, AppServerTransportError>> {
        Box::pin(async move { Ok(self.stdout.recv().await) })
    }
}

/// Test-side handles that script runtime output and observe requests.
struct RuntimeScript {
    stdout: mpsc::UnboundedSender<String>,
    writes: mpsc::UnboundedReceiver<Value>,
}

impl RuntimeScript {
    /// Emits one stdout line from the runtime.
    fn emit(&self, line: &Value) {
        self.stdout
            .send(line.to_string())
            .expect("event loop reads");
    }

    /// Returns the next request written by the event loop.
    async fn next_request(&mut self) -> Value {
        self.writes.recv().await.expect("event loop writes")
    }
}

/// Creates one scripted transport and its controlling handles.
fn scripted_transport() -> (ScriptedTransport, RuntimeScript) {
    let (stdout_tx, stdout_rx) = mpsc::unbounded_channel();
    let (writes_tx, writes_rx) = mpsc::unbounded_channel();

    (
        ScriptedTransport {
            stdout: stdout_rx,
            writes: writes_tx,
        },
        RuntimeScript {
            stdout: stdout_tx,
            writes: writes_rx,
        },
    )
}

/// Returns the string request id of one written JSON-RPC request.
fn request_id(request: &Value) -> String {
    request["id"].as_str().expect("request id").to_string()
}

/// Steers the running turn and answers the resulting `turn/steer` request.
async fn steer_and_answer(
    inbox: &TurnSteeringInbox,
    script: &mut RuntimeScript,
    text: &str,
    answer: impl FnOnce(String) -> Value,
) -> (Value, Result<(), AppServerError>) {
    let acknowledgement = inbox.submit(TurnPrompt::from(text)).expect("open inbox");
    let request = script.next_request().await;
    script.emit(&answer(request_id(&request)));
    let result = acknowledgement.await.expect("acknowledged");

    (request, result)
}

#[tokio::test]
async fn running_turn_delivers_steering_prompts_and_reports_codex_answers() {
    // Arrange
    let folder = tempdir().expect("runtime folder");
    let (mut transport, mut script) = scripted_transport();
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();
    let mut usage_state = usage::UsageTracker::default();
    let inbox = TurnSteeringInbox::new();
    let input = CodexTurnEventLoopInput {
        folder: folder.path(),
        model: model_fixture::CODEX_MODEL.as_str(),
        permission_mode: PermissionMode::AutoEdit,
        prompt: TurnPrompt::from("Implement the task"),
        protocol_profile: ProtocolRequestProfile::SessionTurn,
        reasoning_level: ReasoningLevel::default(),
        speed_mode: SpeedMode::default(),
        steering: Some(&inbox),
        stream_tx,
        thread_id: "thread-1",
        turn_timeout: Duration::from_secs(60),
        usage_state: &mut usage_state,
    };
    let driver = async {
        let turn_start = script.next_request().await;
        let early = inbox
            .submit(TurnPrompt::from("too early"))
            .expect("open inbox")
            .await
            .expect("acknowledged");
        script.emit(&serde_json::json!({
            "id": request_id(&turn_start),
            "result": {"turn": {"id": "turn-1"}}
        }));
        let (accepted_request, accepted) = steer_and_answer(
            &inbox,
            &mut script,
            "also add tests",
            |id| serde_json::json!({"id": id, "result": {"turnId": "turn-1"}}),
        )
        .await;
        let (_, rejected) = steer_and_answer(
            &inbox,
            &mut script,
            "late",
            |id| serde_json::json!({"id": id, "error": {"message": "turn mismatch"}}),
        )
        .await;
        let (_, unexplained) = steer_and_answer(
            &inbox,
            &mut script,
            "later",
            |id| serde_json::json!({"id": id, "error": {}}),
        )
        .await;
        script.stdout.send("  ".to_string()).expect("blank line");
        script.emit(&serde_json::json!({"id": 5, "result": {}}));
        script.emit(&serde_json::json!({
            "method": "turn/completed",
            "params": {"threadId": "thread-1", "turn": {"id": "turn-1", "status": "completed"}}
        }));

        (early, accepted_request, accepted, rejected, unexplained)
    };

    // Act
    let (result, (early, accepted_request, accepted, rejected, unexplained)) =
        tokio::join!(execute_turn_event_loop(&mut transport, input), driver);

    // Assert
    assert_eq!(result.expect("turn completes"), (String::new(), 0, 0));
    assert_eq!(
        early.expect_err("no turn id yet").to_string(),
        "No running turn can receive this message"
    );
    assert_eq!(accepted_request["method"], "turn/steer");
    assert_eq!(accepted_request["params"]["threadId"], "thread-1");
    assert_eq!(accepted_request["params"]["expectedTurnId"], "turn-1");
    assert_eq!(
        accepted_request["params"]["input"][0]["text"],
        "also add tests"
    );
    assert!(accepted.is_ok());
    assert_eq!(rejected.expect_err("rejected").to_string(), "turn mismatch");
    assert_eq!(
        unexplained.expect_err("rejected").to_string(),
        "Codex rejected the message for the running turn"
    );
}

/// Builds the event-loop input of one steerable Codex session turn.
fn steerable_turn_input<'a>(
    folder: &'a std::path::Path,
    inbox: &'a TurnSteeringInbox,
    usage_state: &'a mut usage::UsageTracker,
    stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
) -> CodexTurnEventLoopInput<'a> {
    CodexTurnEventLoopInput {
        folder,
        model: model_fixture::CODEX_MODEL.as_str(),
        permission_mode: PermissionMode::AutoEdit,
        prompt: TurnPrompt::from("Implement the task"),
        protocol_profile: ProtocolRequestProfile::SessionTurn,
        reasoning_level: ReasoningLevel::default(),
        speed_mode: SpeedMode::default(),
        steering: Some(inbox),
        stream_tx,
        thread_id: "thread-1",
        turn_timeout: Duration::from_secs(60),
        usage_state,
    }
}

/// Starts `turn-1` and writes one steer without answering it yet.
async fn start_turn_with_unanswered_steer(
    inbox: &TurnSteeringInbox,
    script: &mut RuntimeScript,
) -> (
    Value,
    tokio::sync::oneshot::Receiver<Result<(), AppServerError>>,
) {
    let turn_start = script.next_request().await;
    script.emit(&serde_json::json!({
        "id": request_id(&turn_start),
        "result": {"turn": {"id": "turn-1"}}
    }));
    let acknowledgement = inbox
        .submit(TurnPrompt::from("also add tests"))
        .expect("open inbox");
    let steer_request = script.next_request().await;
    script.emit(&serde_json::json!({
        "method": "turn/completed",
        "params": {"threadId": "thread-1", "turn": {"id": "turn-1", "status": "completed"}}
    }));

    (steer_request, acknowledgement)
}

#[tokio::test]
async fn completed_turn_still_acknowledges_a_steer_answered_after_completion() {
    // Arrange
    let folder = tempdir().expect("runtime folder");
    let (mut transport, mut script) = scripted_transport();
    let mut usage_state = usage::UsageTracker::default();
    let inbox = TurnSteeringInbox::new();
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();
    let input = steerable_turn_input(folder.path(), &inbox, &mut usage_state, stream_tx);
    let driver = async {
        let (steer_request, acknowledgement) =
            start_turn_with_unanswered_steer(&inbox, &mut script).await;
        script.stdout.send("  ".to_string()).expect("blank line");
        script.emit(&serde_json::json!({"method": "thread/tokenUsage/updated", "params": {}}));
        script.emit(&serde_json::json!({
            "id": request_id(&steer_request),
            "result": {"turnId": "turn-1"}
        }));

        acknowledgement.await.expect("acknowledged")
    };

    // Act
    let (result, acknowledgement) =
        tokio::join!(execute_turn_event_loop(&mut transport, input), driver);

    // Assert
    assert!(result.is_ok());
    assert!(acknowledgement.is_ok());
}

#[tokio::test]
async fn completed_turn_rejects_a_steer_the_runtime_never_answers() {
    // Arrange
    let folder = tempdir().expect("runtime folder");
    let (mut transport, mut script) = scripted_transport();
    let mut usage_state = usage::UsageTracker::default();
    let inbox = TurnSteeringInbox::new();
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();
    let input = steerable_turn_input(folder.path(), &inbox, &mut usage_state, stream_tx);
    let driver = async {
        let (_, acknowledgement) = start_turn_with_unanswered_steer(&inbox, &mut script).await;
        drop(script);

        acknowledgement.await
    };

    // Act
    let (result, acknowledgement) =
        tokio::join!(execute_turn_event_loop(&mut transport, input), driver);

    // Assert
    assert!(result.is_ok());
    assert!(!matches!(acknowledgement, Ok(Ok(()))));
}

#[tokio::test]
async fn restarted_and_compacted_attempts_resend_prompts_the_inbox_accepted() {
    // Arrange
    let folder = tempdir().expect("runtime folder");
    let mut state = CodexRuntimeState::new(
        folder.path().to_path_buf(),
        model_fixture::CODEX_MODEL.as_str().to_string(),
        PermissionMode::AutoEdit,
    );
    state.thread_id = "thread-1".to_string();
    let (mut transport, mut script) = scripted_transport();
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();
    let inbox = TurnSteeringInbox::new();
    let _earlier_ack = inbox
        .submit(TurnPrompt::from("earlier attempt"))
        .expect("open inbox");
    inbox.next().await.acknowledge(Ok(()));
    let options = CodexTurnOptions {
        protocol_profile: ProtocolRequestProfile::SessionTurn,
        reasoning_level: ReasoningLevel::default(),
        speed_mode: SpeedMode::default(),
        steering: Some(&inbox),
    };
    let driver = async {
        let turn_start = script.next_request().await;
        script.emit(&serde_json::json!({
            "id": request_id(&turn_start),
            "result": {"turn": {"id": "turn-1"}}
        }));
        let (_, accepted) = steer_and_answer(
            &inbox,
            &mut script,
            "also add tests",
            |id| serde_json::json!({"id": id, "result": {"turnId": "turn-1"}}),
        )
        .await;
        script.emit(&serde_json::json!({
            "method": "turn/completed",
            "params": {"threadId": "thread-1", "turn": {
                "id": "turn-1",
                "status": "failed",
                "error": {"message": "[contextWindowExceeded] out of room"}
            }}
        }));
        let compact = script.next_request().await;
        script.emit(&serde_json::json!({
            "method": "turn/completed",
            "params": {"turn": {"status": "completed"}}
        }));
        let retried_start = script.next_request().await;
        script.emit(&serde_json::json!({
            "id": request_id(&retried_start),
            "result": {"turn": {"id": "turn-2"}}
        }));
        script.emit(&serde_json::json!({
            "method": "turn/completed",
            "params": {"threadId": "thread-1", "turn": {"id": "turn-2", "status": "completed"}}
        }));

        (turn_start, accepted, compact, retried_start)
    };

    // Act
    let (result, (turn_start, accepted, compact, retried_start)) = tokio::join!(
        run_turn_with_runtime(
            &mut transport,
            &mut state,
            "Implement the task",
            options,
            stream_tx,
        ),
        driver
    );

    // Assert
    assert!(result.is_ok());
    assert!(accepted.is_ok());
    assert_eq!(compact["method"], "thread/compact/start");
    assert_eq!(retried_start["method"], "turn/start");
    assert_eq!(
        turn_start["params"]["input"][0]["text"],
        "Implement the task\n\nearlier attempt"
    );
    assert_eq!(
        retried_start["params"]["input"][0]["text"],
        "Implement the task\n\nearlier attempt\n\nalso add tests"
    );
}

#[test]
fn steer_payload_targets_the_expected_turn() {
    // Arrange
    let prompt = TurnPrompt::from("focus on the parser");

    // Act
    let payload = build_turn_steer_payload("steer-1", "thread-9", "turn-3", &prompt);

    // Assert
    assert_eq!(
        payload,
        serde_json::json!({
            "method": "turn/steer",
            "id": "steer-1",
            "params": {
                "threadId": "thread-9",
                "input": [{"type": "text", "text": "focus on the parser", "text_elements": []}],
                "expectedTurnId": "turn-3"
            }
        })
    );
}
