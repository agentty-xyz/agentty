use super::*;

#[tokio::test]
async fn replay_attempt_owns_archive_and_stops_runtime_on_archive_error() {
    // Arrange
    let folder = tempfile::tempdir().expect("workspace");
    let mut shutdown = TestRuntime::shutdown;
    let mut runtime = TestRuntime {
        model: "model-a".into(),
    };
    let mut request = AppServerTurnRequest {
        folder: folder.path().to_owned(),
        live_transcript: None,
        main_checkout_root: None,
        model: "model-a".into(),
        permission_mode: crate::model::permission::PermissionMode::ReadOnly,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: "Continue".into(),
        request_kind: AgentRequestKind::SessionResume,
        replay_transcript: Some("history".repeat(8192)),
        provider_conversation_id: None,
        persisted_instruction_conversation_id: None,
        reasoning_level: ReasoningLevel::default(),
        session_id: "replay-test".into(),
        speed_mode: crate::model::session::SpeedMode::default(),
    };

    // Act
    let (prompt, archive) = build_attempt_prompt(
        &request,
        true,
        None,
        ProtocolSchemaInstructionMode::TransportSchema,
        &mut shutdown,
        &mut runtime,
    )
    .await
    .expect("archive");
    let live_files = std::fs::read_dir(folder.path()).expect("files").count();
    drop(archive);
    let (native_prompt, native_archive) = build_attempt_prompt(
        &request,
        false,
        Some("thread"),
        ProtocolSchemaInstructionMode::TransportSchema,
        &mut shutdown,
        &mut runtime,
    )
    .await
    .expect("native archive");
    drop(native_archive);
    let remaining_files = std::fs::read_dir(folder.path()).expect("files").count();
    request.folder = folder.path().join("missing");
    let failure = build_attempt_prompt(
        &request,
        true,
        None,
        ProtocolSchemaInstructionMode::TransportSchema,
        &mut shutdown,
        &mut runtime,
    )
    .await;

    // Assert
    assert!(prompt.contains("Session checkpoint"));
    assert!(native_prompt.contains("Earlier temporary history paths have expired"));
    assert!(!native_prompt.contains("Session checkpoint"));

    assert_eq!(live_files, 1);
    assert_eq!(remaining_files, 0);
    assert!(matches!(failure, Err(AppServerError::PromptRender(_))));
    assert_eq!(runtime.model, "stopped");
}

#[test]
fn take_session_returns_stored_runtime() {
    // Arrange
    let sessions = AppServerSessionRegistry::new("Test");
    sessions
        .store_session_or_recover(
            "session-1".to_string(),
            TestRuntime {
                model: "model-a".to_string(),
            },
        )
        .expect("store should succeed");

    // Act
    let session = sessions
        .take_session("session-1")
        .expect("take should succeed");

    // Assert
    assert_eq!(
        session.map(|runtime| runtime.model),
        Some("model-a".to_string())
    );
}

#[tokio::test]
async fn run_turn_with_restart_retry_uses_live_output_on_retry() {
    // Arrange
    let sessions = AppServerSessionRegistry::new("Test");
    let request = AppServerTurnRequest {
        folder: PathBuf::from("/tmp"),
        live_transcript: Some(live_transcript("streamed before crash")),
        main_checkout_root: Some(PathBuf::from("/tmp/project")),
        model: "model-a".to_string(),
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: "Do work".into(),
        request_kind: session_resume_request_kind(),
        replay_transcript: Some("queued snapshot".to_string()),
        provider_conversation_id: None,
        persisted_instruction_conversation_id: None,
        reasoning_level: ReasoningLevel::default(),
        session_id: "session-1".to_string(),
        speed_mode: crate::model::session::SpeedMode::default(),
    };
    let captured_retry_prompt = Arc::new(Mutex::new(String::new()));

    // Act
    let response = run_turn_with_restart_retry(
        &sessions,
        request,
        RuntimeInspector {
            matches_request: |runtime: &TestRuntime, request| runtime.model == request.model,
            pid: |_runtime| Some(42),
            provider_conversation_id: |_runtime| None,
            retain_runtime_after_turn: true,
            restored_context: |_runtime| false,
        },
        ProtocolSchemaInstructionMode::PromptSchema,
        |request: &AppServerTurnRequest| {
            let model = request.model.clone();

            Box::pin(async move { Ok(TestRuntime { model }) })
        },
        {
            let run_count = Arc::new(AtomicUsize::new(0));
            let captured_retry_prompt = Arc::clone(&captured_retry_prompt);
            move |_runtime: &mut TestRuntime, prompt: &TurnPrompt| {
                let attempt = run_count.fetch_add(1, Ordering::SeqCst);
                let prompt = prompt.to_string();
                let captured_retry_prompt = Arc::clone(&captured_retry_prompt);

                Box::pin(async move {
                    if attempt == 0 {
                        return Err(AppServerError::Provider("first failure".to_string()));
                    }

                    if let Ok(mut guard) = captured_retry_prompt.lock() {
                        *guard = prompt;
                    }

                    Ok(("done".to_string(), 7, 3))
                })
            }
        },
        |_runtime: &mut TestRuntime| Box::pin(async {}),
    )
    .await
    .expect("retry should succeed");

    // Assert
    assert!(response.context_reset);
    assert_eq!(response.provider_conversation_id, None);
    let retry_prompt = captured_retry_prompt
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    assert!(
        retry_prompt.contains("streamed before crash"),
        "retry prompt should contain live transcript, not queued snapshot"
    );
    assert!(
        !retry_prompt.contains("queued snapshot"),
        "retry prompt should use live transcript instead of queued snapshot"
    );
}

#[tokio::test]
async fn successful_turn_shuts_down_runtime_when_retention_is_disabled() {
    // Arrange
    let sessions = AppServerSessionRegistry::new("Test");
    let request = AppServerTurnRequest {
        folder: PathBuf::from("/tmp"),
        live_transcript: None,
        main_checkout_root: None,
        model: "model-a".to_string(),
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: "Do work".into(),
        request_kind: session_start_request_kind(),
        replay_transcript: None,
        provider_conversation_id: None,
        persisted_instruction_conversation_id: None,
        reasoning_level: ReasoningLevel::default(),
        session_id: "session-1".to_string(),
        speed_mode: crate::model::session::SpeedMode::default(),
    };
    let shutdown_count = Arc::new(AtomicUsize::new(0));

    // Act
    let response = run_turn_with_restart_retry(
        &sessions,
        request,
        RuntimeInspector {
            matches_request: |runtime: &TestRuntime, request| runtime.model == request.model,
            pid: |_runtime| Some(42),
            provider_conversation_id: |_runtime| Some("gemini-session".to_string()),
            retain_runtime_after_turn: false,
            restored_context: |_runtime| false,
        },
        ProtocolSchemaInstructionMode::PromptSchema,
        |request: &AppServerTurnRequest| {
            let model = request.model.clone();

            Box::pin(async move { Ok(TestRuntime { model }) })
        },
        |_runtime, _prompt| Box::pin(async { Ok(("done".to_string(), 7, 3)) }),
        {
            let shutdown_count = Arc::clone(&shutdown_count);
            move |_runtime| {
                let shutdown_count = Arc::clone(&shutdown_count);

                Box::pin(async move {
                    shutdown_count.fetch_add(1, Ordering::SeqCst);
                })
            }
        },
    )
    .await
    .expect("turn should succeed");
    let stored_runtime = sessions
        .take_session("session-1")
        .expect("session registry should remain available");

    // Assert
    assert_eq!(response.assistant_message, "done");
    assert_eq!(
        response.provider_conversation_id.as_deref(),
        Some("gemini-session")
    );
    assert_eq!(response.pid, None);
    assert_eq!(shutdown_count.load(Ordering::SeqCst), 1);
    assert!(stored_runtime.is_none());
}

#[tokio::test]
async fn run_turn_with_restart_retry_restarts_once_after_first_failure() {
    // Arrange
    let sessions = AppServerSessionRegistry::new("Test");
    let folder = tempfile::tempdir().expect("workspace");
    let history = "previous transcript".repeat(4096);
    let archives = Arc::new(Mutex::new(Vec::new()));
    let request = AppServerTurnRequest {
        folder: folder.path().to_owned(),
        live_transcript: None,
        main_checkout_root: None,
        model: "model-a".to_string(),
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: "Do work".into(),
        request_kind: session_resume_request_kind(),
        replay_transcript: Some(history.clone()),
        provider_conversation_id: None,
        persisted_instruction_conversation_id: None,
        reasoning_level: ReasoningLevel::default(),
        session_id: "session-1".to_string(),
        speed_mode: crate::model::session::SpeedMode::default(),
    };
    let start_count = Arc::new(AtomicUsize::new(0));
    let run_count = Arc::new(AtomicUsize::new(0));
    let shutdown_count = Arc::new(AtomicUsize::new(0));

    // Act
    let response = run_turn_with_restart_retry(
        &sessions,
        request,
        RuntimeInspector {
            matches_request: |runtime: &TestRuntime, request| runtime.model == request.model,
            pid: |_runtime| Some(42),
            provider_conversation_id: |_runtime| None,
            retain_runtime_after_turn: true,
            restored_context: |_runtime| false,
        },
        ProtocolSchemaInstructionMode::PromptSchema,
        {
            let start_count = Arc::clone(&start_count);
            move |request: &AppServerTurnRequest| {
                let start_count = Arc::clone(&start_count);
                let model = request.model.clone();
                assert!(
                    std::fs::read_dir(&request.folder)
                        .expect("archives")
                        .next()
                        .is_none()
                );

                Box::pin(async move {
                    start_count.fetch_add(1, Ordering::SeqCst);

                    Ok(TestRuntime { model })
                })
            }
        },
        {
            let run_count = Arc::clone(&run_count);
            let folder = folder.path().to_owned();
            let archives = Arc::clone(&archives);
            move |_runtime, prompt| {
                let entries = std::fs::read_dir(&folder)
                    .expect("archives")
                    .map(|entry| entry.expect("archive").path())
                    .collect::<Vec<_>>();
                assert_eq!(entries.len(), 1, "only the current attempt archive exists");
                assert_eq!(
                    std::fs::read_to_string(entries[0].join("history.md")).expect("history"),
                    history
                );
                assert!(prompt.contains("Session checkpoint"));
                archives.lock().expect("archives").push(entries[0].clone());
                let attempt = run_count.fetch_add(1, Ordering::SeqCst);

                Box::pin(async move {
                    if attempt == 0 {
                        return Err(AppServerError::Provider("first failure".to_string()));
                    }

                    Ok(("done".to_string(), 7, 3))
                })
            }
        },
        {
            let shutdown_count = Arc::clone(&shutdown_count);
            move |_runtime| {
                let shutdown_count = Arc::clone(&shutdown_count);

                Box::pin(async move {
                    shutdown_count.fetch_add(1, Ordering::SeqCst);
                })
            }
        },
    )
    .await
    .expect("retry should succeed");

    // Assert
    assert_eq!(response.assistant_message, "done");
    assert!(response.context_reset);
    assert_eq!((response.input_tokens, response.output_tokens), (7, 3));
    assert_eq!(response.pid, Some(42));
    assert_eq!(response.provider_conversation_id, None);
    assert_eq!(start_count.load(Ordering::SeqCst), 2);
    assert_eq!(run_count.load(Ordering::SeqCst), 2);
    assert_eq!(shutdown_count.load(Ordering::SeqCst), 1);
    let archives = archives.lock().expect("archives");
    assert_eq!(archives.len(), 2);
    assert_ne!(archives[0], archives[1]);
    assert!(archives.iter().all(|path| !path.exists()));
}

#[tokio::test]
async fn run_turn_with_restart_retry_shutdown_signal_interrupts_in_flight_runtime() {
    // Arrange
    let sessions = AppServerSessionRegistry::new("Test");
    let request = AppServerTurnRequest {
        folder: PathBuf::from("/tmp"),
        live_transcript: None,
        main_checkout_root: None,
        model: "model-a".to_string(),
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: "Do work".into(),
        request_kind: session_resume_request_kind(),
        replay_transcript: Some("previous transcript".to_string()),
        provider_conversation_id: None,
        persisted_instruction_conversation_id: None,
        reasoning_level: ReasoningLevel::default(),
        session_id: "session-1".to_string(),
        speed_mode: crate::model::session::SpeedMode::default(),
    };
    let run_count = Arc::new(AtomicUsize::new(0));
    let shutdown_count = Arc::new(AtomicUsize::new(0));

    // Act
    let result = run_turn_with_restart_retry(
        &sessions,
        request,
        RuntimeInspector {
            matches_request: |runtime: &TestRuntime, request| runtime.model == request.model,
            pid: |_runtime| Some(42),
            provider_conversation_id: |_runtime| None,
            retain_runtime_after_turn: true,
            restored_context: |_runtime| false,
        },
        ProtocolSchemaInstructionMode::PromptSchema,
        |request: &AppServerTurnRequest| {
            let model = request.model.clone();

            Box::pin(async move { Ok(TestRuntime { model }) })
        },
        {
            let run_count = Arc::clone(&run_count);
            let sessions = sessions.clone();
            move |_runtime, _prompt| {
                let run_count = Arc::clone(&run_count);
                let sessions = sessions.clone();

                Box::pin(async move {
                    run_count.fetch_add(1, Ordering::SeqCst);
                    sessions
                        .cancel_active_turn("session-1")
                        .expect("cancel should signal active turn");
                    std::future::pending::<Result<(String, u64, u64), AppServerError>>().await
                })
            }
        },
        {
            let shutdown_count = Arc::clone(&shutdown_count);
            move |_runtime| {
                let shutdown_count = Arc::clone(&shutdown_count);

                Box::pin(async move {
                    shutdown_count.fetch_add(1, Ordering::SeqCst);
                })
            }
        },
    )
    .await;

    // Assert
    assert!(matches!(result, Err(AppServerError::InterruptedByUser(_))));
    assert_eq!(run_count.load(Ordering::SeqCst), 1);
    assert_eq!(shutdown_count.load(Ordering::SeqCst), 1);
}

/// Verifies restored-context retries keep the user prompt while avoiding
/// transcript replay.
#[tokio::test]
async fn run_turn_with_restart_retry_skips_replay_when_runtime_restores_context() {
    // Arrange
    let sessions = AppServerSessionRegistry::new("Test");
    let request = AppServerTurnRequest {
        folder: PathBuf::from("/tmp"),
        live_transcript: None,
        main_checkout_root: None,
        model: "model-a".to_string(),
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: "Do work".into(),
        request_kind: session_resume_request_kind(),
        replay_transcript: Some("previous transcript".to_string()),
        provider_conversation_id: Some("thread-123".to_string()),
        persisted_instruction_conversation_id: None,
        reasoning_level: ReasoningLevel::default(),
        session_id: "session-1".to_string(),
        speed_mode: crate::model::session::SpeedMode::default(),
    };
    let captured_prompt = Arc::new(Mutex::new(String::new()));

    // Act
    let response = run_turn_with_restart_retry(
        &sessions,
        request,
        RuntimeInspector {
            matches_request: |runtime: &TestRuntime, request| runtime.model == request.model,
            pid: |_runtime| Some(24),
            provider_conversation_id: |_runtime| Some("thread-123".to_string()),
            retain_runtime_after_turn: true,
            restored_context: |_runtime| true,
        },
        ProtocolSchemaInstructionMode::PromptSchema,
        |request: &AppServerTurnRequest| {
            let model = request.model.clone();

            Box::pin(async move { Ok(TestRuntime { model }) })
        },
        {
            let captured_prompt = Arc::clone(&captured_prompt);
            move |_runtime: &mut TestRuntime, prompt: &TurnPrompt| {
                let prompt = prompt.to_string();
                let captured_prompt = Arc::clone(&captured_prompt);

                Box::pin(async move {
                    if let Ok(mut guard) = captured_prompt.lock() {
                        *guard = prompt;
                    }

                    Ok(("done".to_string(), 1, 1))
                })
            }
        },
        |_runtime: &mut TestRuntime| Box::pin(async {}),
    )
    .await
    .expect("turn should succeed");

    // Assert
    assert_eq!(response.assistant_message, "done");
    assert!(!response.context_reset);
    assert_eq!(
        response.provider_conversation_id,
        Some("thread-123".to_string())
    );
    assert_eq!(response.pid, Some(24));
    let captured_prompt = captured_prompt
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    assert!(captured_prompt.contains("repository-root-relative POSIX paths"));
    assert!(captured_prompt.ends_with("Do work"));
    assert!(!captured_prompt.contains("previous transcript"));
}
