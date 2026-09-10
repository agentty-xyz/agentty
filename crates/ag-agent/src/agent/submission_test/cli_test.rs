use super::*;

#[test]
fn test_format_one_shot_execution_error_preserves_build_context() {
    // Arrange
    let command_error = CliExecutionError::CommandBuild(
        crate::agent::AgentBackendError::CommandBuild("command".to_string()),
    );
    let stdin_error = CliExecutionError::StdinBuild(crate::agent::AgentBackendError::CommandBuild(
        "stdin".to_string(),
    ));
    let execution_error = CliExecutionError::StdinWrite("write".to_string());

    // Act
    let command_message = format_one_shot_execution_error(command_error);
    let stdin_message = format_one_shot_execution_error(stdin_error);
    let execution_message = format_one_shot_execution_error(execution_error);

    // Assert
    assert_eq!(
        command_message,
        "Failed to build one-shot agent command: command"
    );
    assert_eq!(
        stdin_message,
        "Failed to build one-shot agent stdin payload: stdin"
    );
    assert_eq!(
        execution_message,
        "Failed to execute one-shot agent command: stdin delivery failed: write"
    );
}

#[test]
fn test_one_shot_cli_observer_updates_child_pid_slot() {
    // Arrange
    let child_pid = Arc::new(Mutex::new(None));
    let observer = OneShotCliObserver {
        child_pid: Some(Arc::clone(&child_pid)),
    };

    // Act
    observer.pid_updated(Some(42));
    let active_pid = *child_pid.lock().expect("PID lock should be available");
    observer.stdout_line("collected output");
    observer.pid_updated(None);
    let cleared_pid = *child_pid.lock().expect("PID lock should be available");

    // Assert
    assert_eq!(active_pid, Some(42));
    assert_eq!(cleared_pid, None);
}

#[tokio::test]
async fn test_submit_one_shot_with_backend_reports_signal_interruption() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().returning(|_| {
        let mut command = Command::new("sh");
        command.arg("-c").arg("kill -9 $$");

        Ok(command)
    });

    // Act
    let error = submit_one_shot_with_backend(
        &backend,
        OneShotRequest {
            agent_kind: AgentKind::Codex,
            child_pid: None,
            folder: temp_directory.path().to_path_buf(),
            model: AgentModel::Gpt56Sol,
            permission_mode: PermissionMode::AutoEdit,
            prompt: "Generate title".to_string(),
            request_kind: AgentRequestKind::UtilityPrompt,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::Normal,
        },
    )
    .await
    .expect_err("signal termination should interrupt the one-shot command");

    // Assert
    assert_eq!(error, "One-shot agent command was interrupted");
}

#[tokio::test]
/// Verifies one-shot execution returns the parsed structured answer.
async fn test_submit_one_shot_with_backend_returns_protocol_response() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().returning(|request| {
        assert!(matches!(
            request.request_kind,
            AgentRequestKind::UtilityPrompt
        ));
        assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
        assert_eq!(request.prompt, "Generate title");

        Ok(mock_shell_command(
            r#"{"answer":"Generated title","questions":[]}"#,
            "",
            0,
        ))
    });

    // Act
    let response = submit_one_shot_with_backend(
        &backend,
        OneShotRequest {
            agent_kind: AgentKind::Claude,
            child_pid: None,
            folder: temp_directory.path().to_path_buf(),
            model: AgentModel::ClaudeSonnet5,
            permission_mode: PermissionMode::ReadOnly,
            prompt: "Generate title".to_string(),
            request_kind: AgentRequestKind::UtilityPrompt,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::Normal,
        },
    )
    .await
    .expect("one-shot prompt should succeed");

    // Assert
    assert_eq!(
        response.response.answers(),
        vec!["Generated title".to_string()]
    );
}

#[tokio::test]
/// Verifies one-shot execution does not deadlock when the child delays
/// reading stdin until after it emits early stderr output.
async fn test_submit_one_shot_with_backend_writes_large_stdin_concurrently() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let large_prompt = "x".repeat(512 * 1024);
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().returning(|_| {
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            "printf 'warming up\\n' >&2; sleep 0.1; cat >/dev/null; printf '%s' \
             '{\"answer\":\"done\",\"questions\":[]}'",
        );
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());

        Ok(command)
    });

    // Act
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                agent_kind: AgentKind::Claude,
                child_pid: None,
                folder: temp_directory.path().to_path_buf(),
                model: AgentModel::ClaudeSonnet5,
                permission_mode: PermissionMode::AutoEdit,
                prompt: large_prompt.clone(),
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Normal,
            },
        ),
    )
    .await
    .expect("one-shot prompt should not deadlock")
    .expect("one-shot prompt should succeed");

    // Assert
    assert_eq!(response.response.answers(), vec!["done".to_string()]);
}

#[tokio::test]
/// Verifies one-shot execution streams Claude prompts through stdin so
/// large review requests avoid argv length limits.
async fn test_submit_one_shot_with_backend_writes_prompt_to_stdin() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let capture_path = temp_directory.path().join("stdin.txt");
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().returning({
        let capture_path = capture_path.clone();

        move |_| Ok(stdin_capture_shell_command(&capture_path))
    });

    // Act
    let response = submit_one_shot_with_backend(
        &backend,
        OneShotRequest {
            agent_kind: AgentKind::Claude,
            child_pid: None,
            folder: temp_directory.path().to_path_buf(),
            model: AgentModel::ClaudeSonnet5,
            permission_mode: PermissionMode::AutoEdit,
            prompt: "Generate title".to_string(),
            request_kind: AgentRequestKind::UtilityPrompt,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::Normal,
        },
    )
    .await
    .expect("one-shot prompt should succeed");
    let captured_prompt =
        std::fs::read_to_string(&capture_path).expect("captured stdin payload should exist");

    // Assert
    assert_eq!(response.response.answers(), vec!["captured".to_string()]);
    assert!(captured_prompt.contains("Structured response protocol:"));
    assert!(captured_prompt.contains("Generate title"));
}

#[tokio::test]
/// Verifies a broken stdin pipe does not hide the child exit status or
/// stderr when the backend exits before reading the full prompt.
async fn test_submit_one_shot_with_backend_preserves_exit_error_after_broken_pipe() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let large_prompt = "x".repeat(512 * 1024);
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().returning(|_| {
        let mut command = Command::new("sh");
        command.arg("-c").arg("printf 'auth failed' >&2; exit 7");
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());

        Ok(command)
    });

    // Act
    let error = submit_one_shot_with_backend(
        &backend,
        OneShotRequest {
            agent_kind: AgentKind::Claude,
            child_pid: None,
            folder: temp_directory.path().to_path_buf(),
            model: AgentModel::ClaudeSonnet5,
            permission_mode: PermissionMode::AutoEdit,
            prompt: large_prompt,
            request_kind: AgentRequestKind::UtilityPrompt,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::Normal,
        },
    )
    .await
    .expect_err("one-shot prompt should surface the child exit");

    // Assert
    assert!(error.contains("exit code 7"), "error was: {error}");
    assert!(error.contains("auth failed"), "error was: {error}");
    assert!(
        !error.contains("stdin payload"),
        "stdin write error should not mask child failure: {error}"
    );
}

#[tokio::test]
/// Verifies Claude authentication failures return actionable re-login
/// guidance instead of raw transport output.
async fn test_submit_one_shot_with_backend_surfaces_claude_auth_guidance() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().returning(|_| {
        Ok(mock_shell_command(
            r#"{"type":"error","error":{"type":"authentication_error","message":"OAuth token has expired. Please obtain a new token or refresh your existing token."}}"#,
            "",
            1,
        ))
    });

    // Act
    let error = submit_one_shot_with_backend(
        &backend,
        OneShotRequest {
            agent_kind: AgentKind::Claude,
            child_pid: None,
            folder: temp_directory.path().to_path_buf(),
            model: AgentModel::ClaudeSonnet5,
            permission_mode: PermissionMode::AutoEdit,
            prompt: "Generate title".to_string(),
            request_kind: AgentRequestKind::UtilityPrompt,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::Normal,
        },
    )
    .await
    .expect_err("expired Claude auth should fail");

    // Assert
    assert!(error.contains("One-shot agent command failed because Claude authentication expired"));
    assert!(error.contains("`claude auth login`"));
    assert!(error.contains("`claude auth status`"));
}
