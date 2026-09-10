use super::*;

#[tokio::test]
async fn start_runtime_omits_personality_from_the_process_command() {
    // Arrange
    let runtime_parent = tempdir().expect("create runtime parent");
    let request = AppServerTurnRequest {
        folder: runtime_parent.path().join("missing-runtime"),
        live_transcript: None,
        main_checkout_root: None,
        model: AgentModel::Gpt56Sol.as_str().to_string(),
        permission_mode: PermissionMode::AutoEdit,
        personality: crate::channel::PersonalityPrompt::active(
            "Review carefully.".to_string(),
            true,
        ),
        prompt: TurnPrompt::from("Run the turn"),
        request_kind: crate::channel::AgentRequestKind::SessionStart,
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
        folder: folder.path().to_path_buf(),
        live_transcript: None,
        main_checkout_root: None,
        model: AgentModel::Gpt56Sol.as_str().to_string(),
        permission_mode: PermissionMode::AutoEdit,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: TurnPrompt::from("Run the turn"),
        request_kind: crate::channel::AgentRequestKind::SessionStart,
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
    let model = AgentModel::Gpt56Sol.as_str().to_string();

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
