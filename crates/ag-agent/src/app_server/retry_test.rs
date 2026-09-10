use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ag_protocol::{ProtocolSchemaInstructionMode, TurnPrompt};

use super::{
    RuntimeInspector, build_attempt_prompt, finish_prompt_preparation, run_turn_with_restart_retry,
};
use crate::agent::InstructionDeliveryMode;
use crate::agent::replay::ReplayContext;
use crate::app_server::contract::{AppServerTurnRequest, BorrowedAppServerFuture};
use crate::app_server::error::AppServerError;
use crate::app_server::prompt::{read_latest_replay_transcript, turn_prompt_for_runtime};
use crate::app_server::registry::AppServerSessionRegistry;
use crate::channel::{AgentRequestKind, LiveTranscript};
use crate::model::agent::ReasoningLevel;

#[tokio::test]
async fn size_rejections_shutdown_without_restarting() {
    // Arrange
    for diagnostic in [
        "Input exceeds the maximum length of 1048576 characters.",
        "contextWindowExceeded",
        "context_window_exceeded",
        "context window exceeded",
        "Maximum context length is 8192",
        "Prompt is too long",
    ] {
        let sessions = AppServerSessionRegistry::new("Test");
        let request = AppServerTurnRequest {
            provider_call_budget: None,
            folder: PathBuf::from("."),
            live_transcript: None,
            main_checkout_root: None,
            model: "test".into(),
            permission_mode: crate::PermissionMode::ReadOnly,
            personality: crate::channel::PersonalityPrompt::default(),
            prompt: "input".into(),
            request_kind: AgentRequestKind::UtilityPrompt,
            replay_transcript: None,
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "size-error".into(),
            speed_mode: crate::SpeedMode::default(),
        };
        let starts = AtomicUsize::new(0);
        let shutdowns = AtomicUsize::new(0);

        // Act
        let result = run_turn_with_restart_retry(
            &sessions,
            request,
            RuntimeInspector {
                matches_request: |_: &TestRuntime, _| true,
                pid: |_| None,
                provider_conversation_id: |_| None,
                retain_runtime_after_turn: false,
                restored_context: |_| false,
            },
            ProtocolSchemaInstructionMode::PromptSchema,
            |_| {
                starts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {
                    Ok(TestRuntime {
                        model: "test".into(),
                    })
                })
            },
            |_, _| Box::pin(async move { Err(AppServerError::Provider(diagnostic.into())) }),
            |_| {
                shutdowns.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {})
            },
        )
        .await;

        // Assert
        assert_eq!(
            result.expect_err("operation should fail").to_string(),
            diagnostic
        );
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    }
}

#[derive(Debug)]
struct TestRuntime {
    model: String,
}

impl TestRuntime {
    fn shutdown(&mut self) -> BorrowedAppServerFuture<'_, ()> {
        Box::pin(async move {
            self.model = "stopped".into();
        })
    }
}

#[derive(Debug)]
struct TestLiveTranscript {
    text: String,
}

impl LiveTranscript for TestLiveTranscript {
    fn replay_text(&self) -> Option<String> {
        Some(self.text.clone())
    }
}

fn live_transcript(text: &str) -> Arc<dyn LiveTranscript> {
    Arc::new(TestLiveTranscript {
        text: text.to_string(),
    })
}

fn session_start_request_kind() -> AgentRequestKind {
    AgentRequestKind::SessionStart
}

fn session_resume_request_kind() -> AgentRequestKind {
    AgentRequestKind::SessionResume
}

#[tokio::test]
async fn replay_attempt_owns_archive_and_stops_runtime_on_archive_error() {
    // Arrange
    let folder = tempfile::tempdir().expect("workspace");
    let mut shutdown = TestRuntime::shutdown;
    let mut runtime = TestRuntime {
        model: "model-a".into(),
    };
    let mut request = AppServerTurnRequest {
        provider_call_budget: None,
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

#[test]
fn turn_prompt_for_runtime_adds_repo_root_path_instructions_without_context_reset() {
    // Arrange
    let prompt = "Implement feature";

    // Act
    let turn_prompt = turn_prompt_for_runtime(
        prompt,
        &session_start_request_kind(),
        Some("prior context"),
        InstructionDeliveryMode::BootstrapFull,
        &crate::channel::PersonalityPrompt::default(),
        ProtocolSchemaInstructionMode::PromptSchema,
        std::path::Path::new("/tmp/agentty-wt/session-1"),
    )
    .expect("turn prompt should render");

    // Assert
    assert!(turn_prompt.contains("repository-root-relative POSIX paths"));
    assert!(!turn_prompt.contains("summary"));
    assert!(turn_prompt.ends_with(prompt));
}

#[test]
fn turn_prompt_for_runtime_replays_session_output_after_context_reset_with_path_instructions() {
    // Arrange
    let prompt = "Implement feature";

    // Act
    let turn_prompt = turn_prompt_for_runtime(
        prompt,
        &session_resume_request_kind(),
        Some("assistant: proposed plan"),
        InstructionDeliveryMode::BootstrapWithReplay,
        &crate::channel::PersonalityPrompt::default(),
        ProtocolSchemaInstructionMode::PromptSchema,
        std::path::Path::new("/tmp/agentty-wt/session-1"),
    )
    .expect("turn prompt should render");

    // Assert
    assert!(turn_prompt.contains("repository-root-relative POSIX paths"));
    assert!(turn_prompt.contains("Continue from the supplied session context"));
    assert!(
        turn_prompt
            .contains(r"\<session_transcript> assistant: proposed plan \</session_transcript>")
    );
    assert!(turn_prompt.contains(r"\<user_prompt> Implement feature \</user_prompt>"));
}

#[test]
fn turn_prompt_for_runtime_uses_shared_protocol_wrapper_for_utility_prompts() {
    // Arrange
    let prompt = "Generate title";

    // Act
    let turn_prompt = turn_prompt_for_runtime(
        prompt,
        &AgentRequestKind::UtilityPrompt,
        None,
        InstructionDeliveryMode::BootstrapFull,
        &crate::channel::PersonalityPrompt::default(),
        ProtocolSchemaInstructionMode::PromptSchema,
        std::path::Path::new("/tmp/agentty-wt/session-1"),
    )
    .expect("turn prompt should render");

    // Assert
    assert!(!turn_prompt.contains("summary"));
    assert!(turn_prompt.ends_with(prompt));
}

#[test]
fn read_latest_replay_transcript_prefers_live_buffer_over_snapshot() {
    // Arrange
    let request = AppServerTurnRequest {
        provider_call_budget: None,
        folder: PathBuf::from("/tmp"),
        live_transcript: Some(live_transcript("live content from stream")),
        main_checkout_root: None,
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

    // Act
    let output = read_latest_replay_transcript(&request);

    // Assert
    assert_eq!(output, Some("live content from stream".to_string()));
}

#[test]
fn read_latest_replay_transcript_falls_back_to_snapshot_when_live_buffer_is_empty() {
    // Arrange
    let request = AppServerTurnRequest {
        provider_call_budget: None,
        folder: PathBuf::from("/tmp"),
        live_transcript: Some(live_transcript("")),
        main_checkout_root: None,
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

    // Act
    let output = read_latest_replay_transcript(&request);

    // Assert
    assert_eq!(output, Some("queued snapshot".to_string()));
}

#[test]
fn read_latest_replay_transcript_falls_back_to_snapshot_when_no_live_buffer() {
    // Arrange
    let request = AppServerTurnRequest {
        provider_call_budget: None,
        folder: PathBuf::from("/tmp"),
        live_transcript: None,
        main_checkout_root: None,
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

    // Act
    let output = read_latest_replay_transcript(&request);

    // Assert
    assert_eq!(output, Some("queued snapshot".to_string()));
}

#[test]
fn read_latest_replay_transcript_returns_none_when_both_are_absent() {
    // Arrange
    let request = AppServerTurnRequest {
        provider_call_budget: None,
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

    // Act
    let output = read_latest_replay_transcript(&request);

    // Assert
    assert_eq!(output, None);
}

#[tokio::test]
async fn run_turn_with_restart_retry_uses_live_output_on_retry() {
    // Arrange
    let sessions = AppServerSessionRegistry::new("Test");
    let request = AppServerTurnRequest {
        provider_call_budget: None,
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
        provider_call_budget: None,
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
        provider_call_budget: None,
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
                let model = request.model.clone();
                assert!(
                    std::fs::read_dir(&request.folder)
                        .expect("archives")
                        .next()
                        .is_none()
                );

                start_count.fetch_add(1, Ordering::SeqCst);

                Box::pin(async move { Ok(TestRuntime { model }) })
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
        provider_call_budget: None,
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
        provider_call_budget: None,
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

#[tokio::test]
async fn rendering_failure_shuts_down_runtime_and_removes_replay_archive() {
    // Arrange
    let folder = tempfile::tempdir().expect("workspace");
    let replay = ReplayContext::prepare(folder.path().to_owned(), Some("history".repeat(8192)))
        .await
        .expect("archive");
    assert!(
        std::fs::read_dir(folder.path())
            .expect("archives")
            .next()
            .is_some()
    );
    let mut runtime = TestRuntime {
        model: "running".into(),
    };
    let mut shutdown = TestRuntime::shutdown;

    // Act
    let result = finish_prompt_preparation(
        Err(AppServerError::PromptRender("renderer failed".into())),
        replay,
        &mut shutdown,
        &mut runtime,
    )
    .await;

    // Assert
    assert!(
        matches!(result, Err(AppServerError::PromptRender(message)) if message == "renderer failed")
    );
    assert_eq!(runtime.model, "stopped");
    assert!(
        std::fs::read_dir(folder.path())
            .expect("archives")
            .next()
            .is_none()
    );
}
