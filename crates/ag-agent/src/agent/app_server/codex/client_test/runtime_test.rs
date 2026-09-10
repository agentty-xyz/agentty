use super::*;

#[tokio::test]
async fn runtime_reuse_requires_matching_permission_mode() {
    // Arrange
    let mut runtime = build_stopped_session_runtime("thread-permission");
    let mut request = AppServerTurnRequest {
        folder: runtime.state.folder.clone(),
        live_transcript: None,
        main_checkout_root: None,
        model: runtime.state.model.clone(),
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        persisted_instruction_conversation_id: None,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: TurnPrompt::from("Continue"),
        provider_conversation_id: Some("thread-permission".to_string()),
        reasoning_level: ReasoningLevel::default(),
        replay_transcript: None,
        request_kind: crate::channel::AgentRequestKind::SessionResume,
        session_id: "session-1".to_string(),
        speed_mode: SpeedMode::default(),
    };

    // Act
    let auto_edit_matches = runtime.matches_request(&request);
    request.permission_mode = crate::model::permission::PermissionMode::ReadOnly;
    let read_only_matches = runtime.matches_request(&request);
    runtime.shutdown_runtime().await;

    // Assert
    assert!(auto_edit_matches);
    assert!(!read_only_matches);
}

#[tokio::test]
async fn run_turn_forwards_speed_mode_and_surfaces_transport_failures() {
    // Arrange
    let mut runtime = build_stopped_session_runtime("thread-run-turn");
    let prompt = TurnPrompt::from("Implement the task");
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

    // Act
    let result = CodexRuntimeProvider::run_turn(
        &mut runtime,
        &prompt,
        ProtocolRequestProfile::SessionTurn,
        ReasoningLevel::default(),
        SpeedMode::Fast,
        stream_tx,
    )
    .await;

    // Assert
    let error = result.expect_err("a closed runtime stdin should fail the turn");
    assert!(matches!(error, AppServerError::Transport(_)));
}

#[test]
fn turn_completed_timeout_error_includes_timeout_seconds() {
    // Arrange
    let timeout = Duration::from_secs(9_001);

    // Act
    let error = lifecycle::turn_completed_timeout_error(timeout);

    // Assert
    let error_message = error.to_string();
    assert!(error_message.contains("9001"));
    assert!(error_message.contains("turn/completed"));
}

#[tokio::test]
async fn start_thread_returns_thread_id_from_matching_response() {
    // Arrange
    let folder = tempdir().expect("temporary folder should be created");
    let request_id = Arc::new(Mutex::new(None));
    let mut transport = MockCodexRuntimeTransport::new();
    let mut sequence = Sequence::new();

    transport
        .expect_write_json_line()
        .times(1)
        .in_sequence(&mut sequence)
        .withf({
            let folder = folder.path().to_path_buf();

            move |payload| {
                payload.get("method").and_then(Value::as_str) == Some("thread/start")
                    && payload
                        .get("params")
                        .and_then(|params| params.get("cwd"))
                        .and_then(Value::as_str)
                        == Some(folder.to_string_lossy().as_ref())
            }
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
        .returning(move |_| {
            let response_id = request_id
                .lock()
                .expect("request id mutex should lock")
                .clone()
                .expect("thread/start id should be recorded");

            Box::pin(async move {
                Ok(serde_json::json!({
                    "id": response_id,
                    "result": {"thread": {"id": "thread-123"}}
                })
                .to_string())
            })
        });

    // Act
    let thread_id = lifecycle::start_thread(
        &mut transport,
        folder.path(),
        AgentModel::Gpt56Sol.as_str(),
        crate::model::permission::PermissionMode::AutoEdit,
        ReasoningLevel::default(),
        SpeedMode::default(),
    )
    .await;

    // Assert
    assert_eq!(thread_id.expect("thread should start"), "thread-123");
}

#[tokio::test]
async fn start_or_resume_thread_falls_back_to_thread_start_after_resume_failure() {
    // Arrange
    let folder = tempdir().expect("temporary folder should be created");
    let resume_id = Arc::new(Mutex::new(None));
    let start_id = Arc::new(Mutex::new(None));
    let mut transport = MockCodexRuntimeTransport::new();
    let mut sequence = Sequence::new();

    transport
        .expect_write_json_line()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|payload| payload.get("method").and_then(Value::as_str) == Some("thread/resume"))
        .returning({
            let resume_id = Arc::clone(&resume_id);

            move |payload| {
                remember_request_id(&resume_id, &payload);

                Box::pin(async { Ok(()) })
            }
        });
    transport
        .expect_wait_for_response_line()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(move |_| {
            let response_id = resume_id
                .lock()
                .expect("resume mutex should lock")
                .clone()
                .expect("resume id should be recorded");

            Box::pin(async move {
                Ok(serde_json::json!({
                    "id": response_id,
                    "result": {"thread": {}}
                })
                .to_string())
            })
        });
    transport
        .expect_write_json_line()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|payload| payload.get("method").and_then(Value::as_str) == Some("thread/start"))
        .returning({
            let start_id = Arc::clone(&start_id);

            move |payload| {
                remember_request_id(&start_id, &payload);

                Box::pin(async { Ok(()) })
            }
        });
    transport
        .expect_wait_for_response_line()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(move |_| {
            let response_id = start_id
                .lock()
                .expect("start mutex should lock")
                .clone()
                .expect("start id should be recorded");

            Box::pin(async move {
                Ok(serde_json::json!({
                    "id": response_id,
                    "result": {"thread": {"id": "thread-started"}}
                })
                .to_string())
            })
        });

    // Act
    let thread = lifecycle::start_or_resume_thread(
        &mut transport,
        folder.path(),
        AgentModel::Gpt56Sol.as_str(),
        Some("thread-existing"),
        crate::model::permission::PermissionMode::AutoEdit,
        ReasoningLevel::default(),
        SpeedMode::default(),
    )
    .await;

    // Assert
    assert_eq!(
        thread.expect("thread should be started after resume failure"),
        ("thread-started".to_string(), false)
    );
}

#[tokio::test]
async fn execute_turn_event_loop_answers_user_input_request_without_blocking() {
    // Arrange
    let folder = tempdir().expect("temporary folder should be created");
    let turn_start_id = Arc::new(Mutex::new(None));
    let mut transport = MockCodexRuntimeTransport::new();
    let mut sequence = Sequence::new();
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

    expect_user_input_request_turn(&mut transport, &mut sequence, turn_start_id);

    // Act
    let result = lifecycle::execute_turn_event_loop(
        &mut transport,
        lifecycle::CodexTurnEventLoopInput {
            folder: folder.path(),
            model: AgentModel::Gpt56Sol.as_str(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            prompt: "Implement the task".into(),
            protocol_profile: ProtocolRequestProfile::SessionTurn,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::default(),
            stream_tx,
            thread_id: "thread-1",
            turn_timeout: app_server_transport::TURN_TIMEOUT,
        },
    )
    .await;

    // Assert
    assert_eq!(result.expect("turn should complete"), (String::new(), 0, 0));
}

#[tokio::test]
async fn execute_turn_event_loop_prefers_completed_final_message_over_commentary() {
    // Arrange
    let folder = tempdir().expect("temporary folder should be created");
    let turn_start_id = Arc::new(Mutex::new(None));
    let mut transport = MockCodexRuntimeTransport::new();
    let mut sequence = Sequence::new();
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();
    let final_response = r#"{"answer":"Final focused review.","questions":[]}"#;

    expect_commentary_then_completed_final_turn(
        &mut transport,
        &mut sequence,
        turn_start_id,
        final_response,
    );

    // Act
    let result = lifecycle::execute_turn_event_loop(
        &mut transport,
        lifecycle::CodexTurnEventLoopInput {
            folder: folder.path(),
            model: AgentModel::Gpt56Sol.as_str(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            prompt: "Review the current diff".into(),
            protocol_profile: ProtocolRequestProfile::SessionTurn,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::default(),
            stream_tx,
            thread_id: "thread-1",
            turn_timeout: app_server_transport::TURN_TIMEOUT,
        },
    )
    .await;

    // Assert
    assert_eq!(
        result.expect("turn should return its final answer"),
        (final_response.to_string(), 0, 0)
    );
}

#[test]
fn resolve_turn_usage_prefers_completed_usage_over_stream_usage() {
    // Arrange
    let completed_turn_usage = Some((33, 7));
    let latest_stream_usage = Some((18, 4));

    // Act
    let usage = usage::resolve_turn_usage(completed_turn_usage, latest_stream_usage);

    // Assert
    assert_eq!(usage, (33, 7));
}

#[test]
fn parse_turn_completed_returns_success_for_completed_turn() {
    // Arrange
    let response_value = serde_json::json!({
        "method": "turn/completed",
        "params": {
            "turn": {
                "id": "turn-123",
                "status": "completed"
            }
        }
    });

    // Act
    let turn_result = stream_parser::parse_turn_completed(&response_value, Some("turn-123"));

    // Assert
    assert_eq!(turn_result, Some(Ok(())));
}
