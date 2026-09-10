use super::*;

/// Drains all currently buffered turn events from a test receiver.
#[test]
fn test_map_cli_turn_execution_error_preserves_error_categories() {
    // Arrange
    let command_error = CliExecutionError::CommandBuild(
        crate::agent::AgentBackendError::CommandBuild("command".to_string()),
    );
    let spawn_error = CliExecutionError::Spawn(std::io::Error::other("spawn unavailable"));
    let stdin_error = CliExecutionError::StdinBuild(crate::agent::AgentBackendError::CommandBuild(
        "stdin".to_string(),
    ));
    let io_error = CliExecutionError::StdinWrite("write unavailable".to_string());

    // Act
    let command_message = map_cli_turn_execution_error(command_error).to_string();
    let spawn_message = map_cli_turn_execution_error(spawn_error).to_string();
    let stdin_message = map_cli_turn_execution_error(stdin_error).to_string();
    let io_message = map_cli_turn_execution_error(io_error).to_string();

    // Assert
    assert_eq!(command_message, "Failed to build command: command");
    assert_eq!(spawn_message, "Failed to spawn process: spawn unavailable");
    assert_eq!(
        stdin_message,
        "Failed to build command stdin payload: stdin"
    );
    assert_eq!(io_message, "stdin delivery failed: write unavailable");
}

#[test]
fn test_cli_turn_observer_ignores_blank_progress_text() {
    // Arrange
    let (events, mut event_receiver) = mpsc::unbounded_channel();
    let observer = CliTurnObserver {
        events,
        kind: AgentKind::Codex,
    };

    // Act
    observer.stdout_line(r#"{"type":"item.updated","item":{"type":"reasoning","text":"   "}}"#);

    // Assert
    assert!(event_receiver.try_recv().is_err());
}

#[test]
fn test_build_command_request_uses_agent_facing_prompt_text() {
    // Arrange
    let request = TurnRequest {
        continuation: crate::channel::TurnContinuation::fresh(),
        folder: PathBuf::from("/tmp/session"),
        main_checkout_root: Some(PathBuf::from("/tmp/main")),
        model: "claude-sonnet-5".to_string(),
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: TurnPrompt::from("Review @src/main.rs"),
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::SessionStart,
        response_style: crate::ResponseStyle::default(),
        speed_mode: crate::model::session::SpeedMode::default(),
    };
    let prompt_text = request.prompt.agent_text();

    // Act
    let build_request = build_command_request(&request, &prompt_text);

    // Assert
    assert_eq!(build_request.prompt, "Review \"src/main.rs\"");
    assert_eq!(
        build_request.main_checkout_root,
        Some(std::path::Path::new("/tmp/main"))
    );
}

#[tokio::test]
/// Verifies spawn failure returns `Err` with a descriptive message and
/// does not emit any turn events when the process never starts.
async fn test_run_turn_spawn_failure_returns_err_without_delta() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut mock_backend = MockAgentBackend::new();
    mock_backend
        .expect_build_command()
        .returning(|_| Ok(std::process::Command::new("/no-such-binary-agentty-test")));
    let channel = CliAgentChannel {
        backend: Arc::new(mock_backend),
        kind: AgentKind::Claude,
    };
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let req = make_turn_request(dir.path().to_path_buf());

    // Act
    let result = channel.run_turn("sess-1".to_string(), req, events_tx).await;

    // Assert
    let error_message = result
        .expect_err("expected Err for spawn failure")
        .to_string();
    assert!(
        error_message.contains("Failed to spawn process"),
        "error was: {error_message}"
    );
    assert!(
        events_rx.try_recv().is_err(),
        "no events should be emitted when the process never spawned"
    );
}

#[tokio::test]
/// Verifies kill-by-signal returns `Err` with a `[Stopped]` message and
/// does not emit any loader updates.
async fn test_run_turn_kill_signal_returns_err_without_stopped_delta() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut mock_backend = MockAgentBackend::new();
    mock_backend.expect_build_command().returning(|_| {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg("kill -9 $$");

        Ok(cmd)
    });
    let channel = CliAgentChannel {
        backend: Arc::new(mock_backend),
        kind: AgentKind::Claude,
    };
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let req = make_turn_request(dir.path().to_path_buf());

    // Act
    let result = channel.run_turn("sess-1".to_string(), req, events_tx).await;

    // Assert
    let error_message = result
        .expect_err("expected Err for kill-by-signal")
        .to_string();
    assert!(
        error_message.contains("[Stopped]"),
        "error was: {error_message}"
    );

    // Drain `PidUpdate` events and verify no loader update was emitted.
    while let Ok(event) = events_rx.try_recv() {
        assert!(
            matches!(event, TurnEvent::PidUpdate(_)),
            "only PidUpdate events expected, got: {event:?}"
        );
    }
}

#[tokio::test]
/// Verifies that a clean process exit returns `Ok(TurnResult)` with no
/// context reset (CLI turns never reset context).
async fn test_run_turn_clean_exit_returns_ok_result_without_context_reset() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut mock_backend = MockAgentBackend::new();
    mock_backend.expect_build_command().returning(|_| {
        let mut command = std::process::Command::new("sh");
        command
            .arg("-c")
            .arg("printf '{\"answer\":\"ok\",\"questions\":[]}'");

        Ok(command)
    });
    let channel = CliAgentChannel {
        backend: Arc::new(mock_backend),
        kind: AgentKind::Claude,
    };
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let req = make_turn_request(dir.path().to_path_buf());

    // Act
    let result = channel.run_turn("sess-1".to_string(), req, events_tx).await;

    // Assert
    let turn_result = result.expect("expected Ok for clean exit");
    assert!(!turn_result.context_reset);
}

#[tokio::test]
/// Verifies Claude CLI turns avoid deadlock when the child emits stderr
/// before it starts reading a large stdin prompt.
async fn test_run_turn_writes_large_stdin_concurrently_for_claude() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut mock_backend = MockAgentBackend::new();
    mock_backend.expect_build_command().returning(|_| {
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg(
            "printf 'warming up\\n' >&2; sleep 0.1; cat >/dev/null; printf '%s' \
             '{\"answer\":\"ok\",\"questions\":[]}'",
        );

        Ok(command)
    });
    let channel = CliAgentChannel {
        backend: Arc::new(mock_backend),
        kind: AgentKind::Claude,
    };
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let mut req = make_turn_request(dir.path().to_path_buf());
    req.prompt = "x".repeat(512 * 1024).into();

    // Act
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        channel.run_turn("sess-1".to_string(), req, events_tx),
    )
    .await
    .expect("turn should not deadlock")
    .expect("turn should succeed");

    // Assert
    assert_eq!(result.assistant_message.to_display_text(), "ok");
}

#[tokio::test]
/// Verifies Claude CLI turns stream image-aware prompt text through stdin
/// so large multimodal session prompts do not rely on argv transport.
async fn test_run_turn_writes_prompt_to_stdin_for_claude() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let capture_path = dir.path().join("stdin.txt");
    let image_path = dir.path().join("pasted-image.png");
    std::fs::write(&image_path, b"image-bytes").expect("image should be written");
    let mut mock_backend = MockAgentBackend::new();
    mock_backend.expect_build_command().returning({
        let capture_path = capture_path.clone();

        move |_| Ok(stdin_capture_command(&capture_path))
    });
    let channel = CliAgentChannel {
        backend: Arc::new(mock_backend),
        kind: AgentKind::Claude,
    };
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let mut req = make_turn_request(dir.path().to_path_buf());
    req.prompt = TurnPrompt {
        attachments: vec![TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: image_path.clone(),
        }],
        text: "Review [Image #1]".to_string(),
        text_source: TurnPromptTextSource::UserPrompt,
    };

    // Act
    let result = channel
        .run_turn("sess-1".to_string(), req, events_tx)
        .await
        .expect("turn should succeed");
    let captured_prompt =
        std::fs::read_to_string(&capture_path).expect("captured stdin payload should exist");

    // Assert
    assert_eq!(result.assistant_message.to_display_text(), "ok");
    assert!(captured_prompt.contains("Structured response protocol:"));
    assert!(captured_prompt.contains(image_path.to_string_lossy().as_ref()));
    assert!(!captured_prompt.contains("[Image #1]"));
}

#[tokio::test]
/// Verifies a broken stdin pipe does not hide the backend stderr or exit
/// status when the CLI exits before consuming the full prompt.
async fn test_run_turn_preserves_child_error_after_broken_pipe() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut mock_backend = MockAgentBackend::new();
    mock_backend.expect_build_command().returning(|_| {
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg("printf 'auth failed' >&2; exit 9");

        Ok(command)
    });
    let channel = CliAgentChannel {
        backend: Arc::new(mock_backend),
        kind: AgentKind::Claude,
    };
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let mut req = make_turn_request(dir.path().to_path_buf());
    req.prompt = "x".repeat(512 * 1024).into();

    // Act
    let error = channel
        .run_turn("sess-1".to_string(), req, events_tx)
        .await
        .expect_err("turn should surface the child exit");

    // Assert
    let error_message = error.to_string();
    assert!(
        error_message.contains("auth failed"),
        "error was: {error_message}"
    );
    assert!(
        !error_message.contains("stdin payload"),
        "stdin write error should not mask child failure: {error_message}"
    );
}

#[tokio::test]
async fn cli_archive_failure_stops_before_spawning_provider() {
    // Arrange
    let folder = tempdir().expect("workspace");
    let backend = Arc::new(MockAgentBackend::new());
    let channel = CliAgentChannel::with_backend(backend, AgentKind::Claude);
    let mut request = make_turn_request(folder.path().join("missing"));
    request.continuation = crate::channel::TurnContinuation::replaying("x".repeat(40 * 1024));
    let (events, _receiver) = mpsc::unbounded_channel();

    // Act
    let result = channel.run_turn("session".into(), request, events).await;

    // Assert
    assert!(result.is_err());
}

#[tokio::test]
/// Verifies non-zero CLI turn exits surface actionable Claude
/// re-authentication guidance instead of protocol schema errors.
async fn test_run_turn_returns_claude_auth_guidance_for_expired_token() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut mock_backend = MockAgentBackend::new();
    mock_backend.expect_build_command().times(1).returning(|_| {
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg(
            "printf '%s' \
             '{\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"message\":\"\
             OAuth token has expired. Please obtain a new token or refresh your existing \
             token.\"}}'; exit 1",
        );

        Ok(command)
    });
    let channel = CliAgentChannel {
        backend: Arc::new(mock_backend),
        kind: AgentKind::Claude,
    };
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let req = make_turn_request(dir.path().to_path_buf());

    // Act
    let error_message = channel
        .run_turn("sess-1".to_string(), req, events_tx)
        .await
        .expect_err("expired Claude auth should fail")
        .to_string();

    // Assert
    assert!(error_message.contains("Agent command failed because Claude authentication expired"));
    assert!(error_message.contains("`claude auth login`"));
    assert!(error_message.contains("`claude auth status`"));
}

#[tokio::test]
/// Verifies non-zero CLI turn exits preserve generic stderr details for
/// non-authentication failures.
async fn test_run_turn_returns_exit_error_for_non_zero_status() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut mock_backend = MockAgentBackend::new();
    mock_backend.expect_build_command().times(1).returning(|_| {
        let mut command = std::process::Command::new("sh");
        command
            .arg("-c")
            .arg("printf '%s' 'assist failed' >&2; exit 7");

        Ok(command)
    });
    let channel = CliAgentChannel {
        backend: Arc::new(mock_backend),
        kind: AgentKind::Claude,
    };
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let req = make_turn_request(dir.path().to_path_buf());

    // Act
    let error_message = channel
        .run_turn("sess-1".to_string(), req, events_tx)
        .await
        .expect_err("non-zero exit should fail")
        .to_string();

    // Assert
    assert!(error_message.contains("Agent command failed with exit code 7"));
    assert!(error_message.contains("assist failed"));
}

#[tokio::test]
/// Verifies CLI channels surface only transient loader text while the
/// final assistant response is returned at turn completion.
async fn test_run_turn_surfaces_only_loader_updates_for_strict_protocol_provider() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut mock_backend = MockAgentBackend::new();
    mock_backend.expect_build_command().returning(|_| {
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg(concat!(
            r#"echo '{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash"}]}}';"#,
            r#"echo '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"streamed fragment"}]}}';"#,
            r#"echo '{"result":"{\"answer\":\"final answer\",\"questions\":[]}","usage":{"input_tokens":5,"output_tokens":3}}'"#,
        ));

        Ok(command)
    });
    let channel = CliAgentChannel {
        backend: Arc::new(mock_backend),
        kind: AgentKind::Claude,
    };
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let req = make_turn_request(dir.path().to_path_buf());

    // Act
    let result = channel
        .run_turn("sess-1".to_string(), req, events_tx)
        .await
        .expect("turn should succeed");

    // Assert
    let mut saw_loader_update = false;
    while let Ok(event) = events_rx.try_recv() {
        if matches!(event, TurnEvent::ThoughtDelta(_)) {
            saw_loader_update = true;
        }
    }
    assert!(saw_loader_update, "loader updates should be streamed live");
    assert_eq!(result.assistant_message.to_display_text(), "final answer");
}
