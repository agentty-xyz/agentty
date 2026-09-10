use std::sync::Arc;
use std::time::Duration;

use ag_protocol::TurnPrompt;
use tempfile::tempdir;
use tokio::sync::mpsc;

use super::support::make_turn_request;
use crate::agent::{AgentBackend, BuildCommandRequest, MockAgentBackend};
use crate::channel::cli::{
    CliAgentChannel, execute_cli_repair_command, execute_cli_repair_turn,
    parse_or_repair_cli_response,
};
use crate::channel::contract::{AgentChannel, AgentRequestKind, TurnEvent};
use crate::model::agent::{AgentKind, ReasoningLevel};

#[tokio::test]
async fn test_parse_or_repair_cli_response_reports_repair_transport_failure() {
    // Arrange
    let folder = tempdir().expect("failed to create temp dir");
    let request = make_turn_request(folder.path().to_path_buf());
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().returning(|_| {
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg("exit 7");

        Ok(command)
    });
    let backend: Arc<dyn AgentBackend> = Arc::new(backend);
    let (events, mut event_receiver) = mpsc::unbounded_channel();

    // Act
    let error = parse_or_repair_cli_response(
        AgentKind::Codex,
        "not a protocol response",
        &request,
        &backend,
        &events,
    )
    .await
    .expect_err("a failing repair transport should fail the turn");

    // Assert
    assert!(
        error
            .to_string()
            .contains("protocol repair transport failed: repair process exited with code 7"),
        "unexpected repair error: {error}"
    );
    assert!(matches!(
        event_receiver.try_recv(),
        Ok(TurnEvent::ThoughtDelta(_))
    ));
}

#[tokio::test]
async fn test_execute_cli_repair_turn_reports_non_zero_exit() {
    // Arrange
    let folder = tempdir().expect("failed to create temp dir");
    let request = make_turn_request(folder.path().to_path_buf());
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().returning(|_| {
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg("exit 7");

        Ok(command)
    });

    // Act
    let error = execute_cli_repair_turn(&backend, AgentKind::Codex, &request, "repair")
        .await
        .expect_err("repair command should fail");

    // Assert
    assert_eq!(error, "repair process exited with code 7");
}

#[tokio::test]
async fn test_execute_cli_repair_turn_reports_signal() {
    // Arrange
    let folder = tempdir().expect("failed to create temp dir");
    let request = make_turn_request(folder.path().to_path_buf());
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().returning(|_| {
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg("kill -9 $$");

        Ok(command)
    });

    // Act
    let error = execute_cli_repair_turn(&backend, AgentKind::Codex, &request, "repair")
        .await
        .expect_err("repair command should be interrupted");

    // Assert
    assert_eq!(error, "repair process was interrupted by signal 9");
}

#[tokio::test]
async fn test_execute_cli_repair_turn_cleans_up_stdin_writer_after_timeout() {
    // Arrange
    let folder = tempdir().expect("failed to create temp dir");
    let request_kind = AgentRequestKind::SessionStart;
    let repair_prompt = "repair ".repeat(200_000);
    let prompt_payload = TurnPrompt::from_agent_data(repair_prompt.clone());
    let build_request = BuildCommandRequest {
        attachments: &prompt_payload.attachments,
        folder: folder.path(),
        main_checkout_root: None,
        replay_transcript: None,
        model: "test-model",
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        personality_prompt: None,
        prompt: &repair_prompt,
        reasoning_level: ReasoningLevel::default(),
        request_kind: &request_kind,
        speed_mode: crate::model::session::SpeedMode::default(),
    };
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().returning(|request| {
        assert!(request.prompt.len() > 1_000_000);
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg("while :; do :; done");

        Ok(command)
    });

    // Act
    let error = execute_cli_repair_command(
        &backend,
        AgentKind::Claude,
        build_request,
        Duration::from_millis(20),
    )
    .await
    .expect_err("repair command should time out");

    // Assert
    assert!(error.starts_with("repair process timed out after"));
}

#[tokio::test]
/// Verifies strict turn parsing recovers one trailing protocol payload
/// when Claude prepends extra prose before the final JSON object.
async fn test_run_turn_recovers_wrapped_structured_output_for_claude() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut mock_backend = MockAgentBackend::new();
    mock_backend.expect_build_command().returning(|_| {
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg(concat!(
            "printf '%s\\n' 'Now I have the full context.';",
            "printf '%s' '{\"answer\":\"ok\",\"questions\":[]}'",
        ));

        Ok(command)
    });
    let channel = CliAgentChannel {
        backend: Arc::new(mock_backend),
        kind: AgentKind::Claude,
    };
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let req = make_turn_request(dir.path().to_path_buf());

    // Act
    let result = channel
        .run_turn("sess-1".to_string(), req, events_tx)
        .await
        .expect("turn should succeed");

    // Assert
    assert_eq!(result.assistant_message.to_answer_display_text(), "ok");
}

#[tokio::test]
/// Verifies Claude turns surface invalid structured output after both the
/// original parse and the protocol-repair retry fail.
async fn test_run_turn_returns_error_for_invalid_structured_output_for_claude() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut mock_backend = MockAgentBackend::new();
    mock_backend
        .expect_build_command()
        .times(2)
        .returning(|request| {
            assert!(matches!(
                request.request_kind,
                AgentRequestKind::SessionStart
            ));

            let mut command = std::process::Command::new("sh");
            command.arg("-c").arg("printf 'plain non-json response'");

            Ok(command)
        });
    let channel = CliAgentChannel {
        backend: Arc::new(mock_backend),
        kind: AgentKind::Claude,
    };
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let req = make_turn_request(dir.path().to_path_buf());

    // Act
    let error = channel
        .run_turn("sess-1".to_string(), req, events_tx)
        .await
        .expect_err("invalid structured output should fail");

    // Assert
    let error_message = error.to_string();
    assert!(error_message.contains("did not match the required JSON schema"));
    assert!(!error_message.contains("plain non-json response"));
}

#[tokio::test]
async fn repair_receives_complete_response_and_rejects_oversize_before_execution() {
    // Arrange
    let folder = tempdir().expect("workspace");
    let request = make_turn_request(folder.path().to_owned());
    let mut backend = MockAgentBackend::new();
    backend
        .expect_build_command()
        .times(1)
        .returning(|request| {
            assert!(request.prompt.contains("preserved tail"));
            assert_eq!(
                request.permission_mode,
                crate::model::permission::PermissionMode::ReadOnly
            );
            let mut command = std::process::Command::new("sh");
            command
                .arg("-c")
                .arg(r#"printf '{"answer":"repaired","questions":[]}'"#);

            Ok(command)
        });
    let backend: Arc<dyn AgentBackend> = Arc::new(backend);
    let (events, _receiver) = mpsc::unbounded_channel();
    let malformed = format!("{} preserved tail", "x".repeat(2048));

    // Act
    let repaired =
        parse_or_repair_cli_response(AgentKind::Claude, &malformed, &request, &backend, &events)
            .await
            .expect("repair");
    let rejected = parse_or_repair_cli_response(
        AgentKind::Claude,
        &"x".repeat(128 * 1024 + 1),
        &request,
        &backend,
        &events,
    )
    .await;

    // Assert
    assert_eq!(repaired.answer, "repaired");
    assert!(
        rejected
            .expect_err("too large")
            .to_string()
            .contains("lossless repair limit")
    );
}

#[tokio::test]
/// Verifies Claude turns recover valid output when the initial parse fails
/// but the protocol-repair retry returns valid protocol JSON.
async fn test_run_turn_recovers_valid_output_via_protocol_repair_for_claude() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let call_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut mock_backend = MockAgentBackend::new();
    mock_backend.expect_build_command().times(2).returning({
        let counter = Arc::clone(&call_counter);

        move |_| {
            let call_number = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut command = std::process::Command::new("sh");

            if call_number == 0 {
                command.arg("-c").arg("printf 'plain non-json response'");
            } else {
                command
                    .arg("-c")
                    .arg(r#"printf '{"answer":"Repaired response","questions":[]}'"#);
            }

            Ok(command)
        }
    });
    let channel = CliAgentChannel {
        backend: Arc::new(mock_backend),
        kind: AgentKind::Claude,
    };
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let req = make_turn_request(dir.path().to_path_buf());

    // Act
    let result = channel
        .run_turn("sess-1".to_string(), req, events_tx)
        .await
        .expect("repair retry should succeed");

    // Assert
    assert_eq!(
        result.assistant_message.to_display_text(),
        "Repaired response"
    );
}
