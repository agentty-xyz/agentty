use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_agent::{
    AgentError, AgentRequestKind, MockAgentChannel, PermissionMode, TurnContinuation, TurnRequest,
    TurnResult,
};
use ag_forge as forge;
use ag_git::MockGitClient;
use ag_protocol::AgentResponse;
use tempfile::tempdir;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::instrument::WithSubscriber;

use super::super::super::post_turn::{TurnPersonalityPersistence, build_assistant_message_content};
use super::super::super::turn::{
    resolve_turn_personality, run_channel_turn, run_turn_with_cancellation, terminate_child_process,
};
use super::super::{
    REBASE_OPERATION_KIND, ScheduledSessionCommand, ScheduledSessionWork, SessionCommand,
    SessionWorkerContext, SessionWorkerRebaseAssistClient, SessionWorkerService, TurnMetadata,
};
use super::support::{
    apply_worker_turn_result, auto_commit_one_shot_client, cancel_token_after_short_delay,
    default_turn_metadata, empty_transcript, expect_clean_main_checkout_snapshot,
    expect_safe_auto_push_state, insert_in_progress_research_session,
    insert_in_progress_test_session, mock_fs_client_with_existing_directories,
    mock_git_client_detecting_main_repo, persist_test_personality_state, queue_test_context,
    queued_message, research_title_one_shot_client, resume_command, seed_recovery_test_operation,
    successful_turn_result, transcript_text, turn_prompt_with_attachment,
};
use crate::app::AppEvent;
use crate::app::branch_publish::BranchPublishTaskSession;
use crate::app::session::SessionError;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel};
use crate::domain::question::QuestionItem;
use crate::domain::session::{PublishedBranchSyncStatus, SessionId, Status};
use crate::infra::db::AppRepositories;
use crate::infra::fs;
use crate::infra::personality::{MockPersonalityCatalogClient, RealPersonalityCatalogClient};

#[tokio::test]
/// Verifies unfinished operations remain executable when cancel has not
/// been requested.
async fn test_should_skip_worker_command_without_cancel_request() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert");
    db.sessions()
        .insert_session(
            "sess1",
            "gemini-3.8-flash",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
    db.operations()
        .insert_session_operation("op-1", "sess1", "reply")
        .await
        .expect("failed to insert session operation");

    let mut mock_channel = MockAgentChannel::new();
    mock_channel
        .expect_shutdown_session()
        .returning(|_| Box::pin(async { Ok(()) }));

    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(mock_channel),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: base_dir.path().to_path_buf(),
        fs_client: Arc::new(fs::MockFsClient::new()),
        git_client: Arc::new(MockGitClient::new()),
        transcript: empty_transcript(),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        status: Arc::new(Mutex::new(Status::InProgress)),
    };

    // Act
    let should_skip = SessionWorkerService::should_skip_worker_command(&context, "op-1").await;
    let is_unfinished = db
        .operations()
        .is_session_operation_unfinished("op-1")
        .await
        .expect("failed to check operation status");

    // Assert
    assert!(!should_skip);
    assert!(is_unfinished);
}

#[tokio::test]
/// Verifies cancel requests skip queued operations before execution and
/// mark them canceled.
async fn test_should_skip_worker_command_when_cancel_is_requested() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert");
    db.sessions()
        .insert_session(
            "sess1",
            "gemini-3.8-flash",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
    db.operations()
        .insert_session_operation("op-1", "sess1", "reply")
        .await
        .expect("failed to insert session operation");
    db.operations()
        .request_cancel_for_session_operations("sess1")
        .await
        .expect("failed to request cancel");

    let mut mock_channel = MockAgentChannel::new();
    mock_channel
        .expect_shutdown_session()
        .returning(|_| Box::pin(async { Ok(()) }));

    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(mock_channel),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: base_dir.path().to_path_buf(),
        fs_client: Arc::new(fs::MockFsClient::new()),
        git_client: Arc::new(MockGitClient::new()),
        transcript: empty_transcript(),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        status: Arc::new(Mutex::new(Status::InProgress)),
    };

    // Act
    let should_skip = SessionWorkerService::should_skip_worker_command(&context, "op-1").await;
    let is_unfinished = db
        .operations()
        .is_session_operation_unfinished("op-1")
        .await
        .expect("failed to check operation status");

    // Assert
    assert!(should_skip);
    assert!(!is_unfinished);
}

#[tokio::test]
/// Verifies a new operation created after a session-level cancel request
/// is not skipped. The operation-scoped check ensures stale cancel flags
/// on older operations do not block newly enqueued work.
async fn test_should_skip_worker_command_allows_new_operation_after_cancel() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert");
    db.sessions()
        .insert_session(
            "sess1",
            "gemini-3.8-flash",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");

    // Old operation that gets cancelled.
    db.operations()
        .insert_session_operation("op-old", "sess1", "reply")
        .await
        .expect("failed to insert old operation");
    db.operations()
        .mark_session_operation_running("op-old")
        .await
        .expect("failed to mark old operation running");
    db.operations()
        .request_cancel_for_session_operations("sess1")
        .await
        .expect("failed to request cancel");

    // New operation created after the cancel request — its
    // `cancel_requested` defaults to 0.
    db.operations()
        .insert_session_operation("op-new", "sess1", "reply")
        .await
        .expect("failed to insert new operation");

    let mut mock_channel = MockAgentChannel::new();
    mock_channel
        .expect_shutdown_session()
        .returning(|_| Box::pin(async { Ok(()) }));

    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(mock_channel),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: base_dir.path().to_path_buf(),
        fs_client: Arc::new(fs::MockFsClient::new()),
        git_client: Arc::new(MockGitClient::new()),
        transcript: empty_transcript(),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        status: Arc::new(Mutex::new(Status::InProgress)),
    };

    // Act — the new operation should proceed despite the old
    // cancelled operation still being in 'running' state.
    let should_skip = SessionWorkerService::should_skip_worker_command(&context, "op-new").await;

    // Assert
    assert!(
        !should_skip,
        "new operation should not be skipped by stale cancel on older operation"
    );
}

#[test]
/// Ensures assistant message content prefers `answer` messages when
/// available.
fn test_build_assistant_message_content_prefers_answer_messages() {
    // Arrange
    let response = AgentResponse {
        answer: "Implemented the fix.".to_string(),
        questions: vec![QuestionItem::new("Need me to run tests?")],
        review_comment_outcomes: Vec::new(),
        subtasks: Vec::new(),
        verification_verdicts: Vec::new(),
    };

    // Act
    let message_content = build_assistant_message_content(&response);

    // Assert
    assert_eq!(
        message_content,
        Some("Implemented the fix.\n\n".to_string())
    );
}

#[test]
/// Ensures blank protocol messages do not append empty transcript messages.
fn test_build_assistant_message_content_returns_none_for_blank_messages() {
    // Arrange
    let response = AgentResponse {
        answer: String::new(),
        questions: vec![QuestionItem::new("\n")],
        review_comment_outcomes: Vec::new(),
        subtasks: Vec::new(),
        verification_verdicts: Vec::new(),
    };

    // Act
    let message_content = build_assistant_message_content(&response);

    // Assert
    assert_eq!(message_content, None);
}

#[tokio::test]
/// Verifies failed background auto-push attempts emit one durable notice
/// for atomic reducer promotion.
async fn test_apply_turn_result_reports_background_push_failures() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session(
            "sess1",
            "gemini-3.8-flash",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(true) }));
    expect_safe_auto_push_state(&mut mock_git_client);
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .once()
        .returning(|_, _| {
            Box::pin(async {
                Err(ag_git::GitError::CommandFailed {
                    command: "git push origin wt/session-id".to_string(),
                    stderr:
                        "fatal: could not read username for 'https://github.com/openai/agentty': \
                         terminal prompts disabled"
                            .to_string(),
                })
            })
        });
    let transcript = empty_transcript();
    let context = SessionWorkerContext {
        app_event_tx,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(MockAgentChannel::new()),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: base_dir.path().join("sess1"),
        fs_client: Arc::new(fs::MockFsClient::new()),
        git_client: Arc::new(mock_git_client),
        transcript: Arc::clone(&transcript),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent,
        status: Arc::new(Mutex::new(Status::InProgress)),
    };
    let turn_result = Ok(successful_turn_result("Implemented the change."));

    // Act
    let turn_metadata = TurnMetadata {
        published_upstream_ref: Some("origin/wt/session-id".to_string()),
        review_comment_thread_ids: Vec::new(),
        session_agent,
    };
    let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
        .await
        .expect("turn result should succeed");
    let sync_events = tokio::time::timeout(Duration::from_secs(1), async {
        let mut sync_events = Vec::new();
        while sync_events.len() < 2 {
            let event = app_event_rx.recv().await.expect("missing app event");
            if let AppEvent::PublishedBranchSyncUpdated {
                persistent_notice,
                sync_status,
                ..
            } = event
            {
                sync_events.push((sync_status, persistent_notice));
            }
        }

        sync_events
    })
    .await
    .expect("timed out waiting for sync events");

    // Assert
    assert_eq!(status, Status::Review);
    assert!(matches!(
        sync_events.as_slice(),
        [
            (PublishedBranchSyncStatus::InProgress, None),
            (PublishedBranchSyncStatus::Failed, Some(_))
        ]
    ));
    let failure_notice = sync_events[1]
        .1
        .as_deref()
        .expect("failed sync should promote one durable notice");
    assert!(failure_notice.contains("[Branch Push Error]"));
    assert!(failure_notice.contains("gh auth login"));
}

#[test]
fn test_preparation_reservation_tracks_command_lifetime_and_overlapping_claims() {
    // Arrange
    let mut service = SessionWorkerService::new();
    let id = SessionId::from("child");
    let mut first = ScheduledSessionCommand::immediate(resume_command("workspace:child"));
    let mut second = ScheduledSessionCommand::immediate(resume_command("workspace:child"));
    let mut ordinary = ScheduledSessionCommand::immediate(resume_command("ordinary"));

    // Act
    service.reserve_preparation_command(&id, &mut ordinary);
    service.reserve_preparation_command(&id, &mut first);
    service.reserve_preparation_command(&id, &mut second);
    drop(first);

    // Assert
    assert!(ordinary.preparation_reservation.is_none());
    assert!(service.has_preparation_reservation(&id));

    // Act: abandoning the last queued command releases the claim.
    drop(second);

    // Assert
    assert!(!service.has_preparation_reservation(&id));

    // Act: retry after an abandoned command.
    let mut retry = ScheduledSessionCommand::immediate(resume_command("workspace:child"));
    service.reserve_preparation_command(&id, &mut retry);

    // Assert
    assert!(service.has_preparation_reservation(&id));
}

#[tokio::test]
async fn test_run_channel_turn_finalizes_invalid_permission_setup_failure() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("sess1", "gemini-3.8-flash", "main", "Question", project_id)
        .await
        .expect("failed to insert question session");
    db.sessions()
        .update_session_questions("sess1", r#"[{"text":"Continue?"}]"#)
        .await
        .expect("failed to persist questions");
    sqlx::query("UPDATE session SET permission_mode = 'invalid' WHERE id = 'sess1'")
        .execute(&pool)
        .await
        .expect("failed to corrupt permission mode");

    let attachment_path = crate::app::agentty_home()
        .join("tmp")
        .join("sess1")
        .join("images")
        .join("image-1.png");
    let image_directory = attachment_path
        .parent()
        .expect("attachment should have a parent")
        .to_path_buf();
    let mut fs_client = mock_fs_client_with_existing_directories();
    let expected_attachment_path = attachment_path.clone();
    fs_client
        .expect_remove_file()
        .once()
        .withf(move |path| path == &expected_attachment_path)
        .returning(|_| Box::pin(async { Ok(()) }));
    fs_client
        .expect_remove_dir()
        .once()
        .withf(move |path| path == &image_directory)
        .returning(|_| Box::pin(async { Ok(()) }));

    let mut mock_git_client = MockGitClient::new();
    expect_clean_main_checkout_snapshot(&mut mock_git_client, base_dir.path().join("main"));
    let mut mock_channel = MockAgentChannel::new();
    mock_channel.expect_run_turn().times(0);
    let transcript = empty_transcript();
    let status = Arc::new(Mutex::new(Status::Question));
    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(mock_channel),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: base_dir.path().to_path_buf(),
        fs_client: Arc::new(fs_client),
        git_client: Arc::new(mock_git_client),
        transcript: Arc::clone(&transcript),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent: AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash),
        status: Arc::clone(&status),
    };
    let prompt = turn_prompt_with_attachment(attachment_path);

    // Act
    let result = run_channel_turn(
        &context,
        auto_commit_one_shot_client(),
        default_turn_metadata(),
        AgentRequestKind::SessionResume,
        None,
        prompt,
    )
    .await;
    let persisted_session = db
        .sessions()
        .load_session("sess1")
        .await
        .expect("session should load")
        .expect("session should exist");

    // Assert
    let error = result.expect_err("invalid permission mode should fail the turn");
    assert!(
        error
            .to_string()
            .contains("Unknown permission mode: invalid")
    );
    assert!(transcript_text(&transcript).contains("Unknown permission mode: invalid"));
    assert_eq!(
        *status.lock().expect("status lock poisoned"),
        Status::Review
    );
    assert_eq!(persisted_session.status, "Review");
}

#[tokio::test]
/// Verifies the worker's `select!` cancellation path gracefully stops a
/// running turn through `shutdown_session` and returns the `[Stopped]`
/// error text when the cancel token is cancelled during `run_channel_turn`.
async fn test_run_channel_turn_returns_stopped_when_cancel_token_fires() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_in_progress_research_session(&db).await;
    db.sessions()
        .update_session_provisional_title("sess1", "test prompt")
        .await
        .expect("failed to persist provisional research title");

    let mut mock_channel = MockAgentChannel::new();
    mock_channel
        .expect_run_turn()
        .withf(|_session_id, request, _events| {
            request.permission_mode == PermissionMode::ReadOnly
                && !request.prompt.text.contains("# Read Only Mode")
        })
        .returning(|_session_id, _req, _events| {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_hours(1)).await;
                unreachable!("should be cancelled before completing")
            })
        });
    mock_channel
        .expect_shutdown_session()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));

    let mut mock_git_client = MockGitClient::new();
    expect_clean_main_checkout_snapshot(&mut mock_git_client, base_dir.path().join("main"));

    let cancel_token = Arc::new(Mutex::new(CancellationToken::new()));
    let transcript = empty_transcript();
    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::clone(&cancel_token),
        channel: Arc::new(mock_channel),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: base_dir.path().to_path_buf(),
        fs_client: Arc::new(mock_fs_client_with_existing_directories()),
        git_client: Arc::new(mock_git_client),
        transcript: Arc::clone(&transcript),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        status: Arc::new(Mutex::new(Status::InProgress)),
    };

    cancel_token_after_short_delay(Arc::clone(&cancel_token));

    // Act
    let result = run_channel_turn(
        &context,
        research_title_one_shot_client(),
        default_turn_metadata(),
        AgentRequestKind::SessionStart,
        None,
        "test prompt".into(),
    )
    .await;

    // Assert
    let error_message = result.expect_err("should return an error").to_string();
    assert!(
        error_message.contains("[Stopped]"),
        "error should contain [Stopped], got: {error_message}"
    );
    let output_text = transcript_text(&transcript);
    assert!(
        output_text.contains("[Stopped]"),
        "stopped message should be appended to transcript, got: {output_text}"
    );
    assert_eq!(
        *context.status.lock().expect("status lock poisoned"),
        Status::InProgress,
        "stopped turn worker must not fall back to Review before the UI cancellation path \
         finalizes Canceled"
    );
    let sessions = db
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    assert_eq!(
        sessions[0].status, "InProgress",
        "stopped turn worker must not persist Review and trigger automatic focused review"
    );
    assert_eq!(
        sessions[0].title.as_deref(),
        Some("Inspect architecture boundaries")
    );
}

#[tokio::test]
/// Verifies a read-only chat turn carries its persisted permission after a
/// previous turn's cancelled token is replaced.
async fn test_run_channel_turn_proceeds_read_only_after_previous_cancellation() {
    // Arrange — pre-cancel the token to simulate a previous turn's
    // cancellation. `run_channel_turn` swaps in a fresh token so the
    // stale cancellation is discarded.
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session(
            "sess1",
            "gemini-3.8-flash",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
    db.sessions()
        .update_session_permission_mode("sess1", PermissionMode::ReadOnly)
        .await
        .expect("failed to set read-only permission mode");

    let mut mock_channel = MockAgentChannel::new();
    mock_channel
        .expect_run_turn()
        .once()
        .withf(|_session_id, request, _events| {
            request.permission_mode == PermissionMode::ReadOnly
                && request.prompt.text.contains("# Read Only Mode")
        })
        .returning(|_session_id, _req, _events| {
            Box::pin(async {
                Ok(TurnResult {
                    assistant_message: AgentResponse {
                        answer: "done".to_string(),
                        questions: Vec::new(),
                        review_comment_outcomes: Vec::new(),
                        subtasks: Vec::new(),
                        verification_verdicts: Vec::new(),
                    },
                    context_reset: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    provider_conversation_id: None,
                })
            })
        });

    let mut mock_git_client = mock_git_client_detecting_main_repo(base_dir.path().join("main"));
    mock_git_client
        .expect_tracked_worktree_status()
        .times(2)
        .returning(|_| Box::pin(async { Ok(String::new()) }));
    mock_git_client
        .expect_diff()
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    mock_git_client
        .expect_is_worktree_clean()
        .returning(|_| Box::pin(async { Ok(true) }));

    // Pre-cancel the token to simulate a previous turn's cancellation.
    let stale_token = CancellationToken::new();
    stale_token.cancel();
    let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);

    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(stale_token)),
        channel: Arc::new(mock_channel),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: base_dir.path().to_path_buf(),
        fs_client: Arc::new(mock_fs_client_with_existing_directories()),
        git_client: Arc::new(mock_git_client),
        transcript: empty_transcript(),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent,
        status: Arc::new(Mutex::new(Status::InProgress)),
    };

    // Act — the turn should complete normally because
    // `run_channel_turn` swaps in a fresh token.
    let result = run_channel_turn(
        &context,
        auto_commit_one_shot_client(),
        default_turn_metadata(),
        AgentRequestKind::SessionStart,
        None,
        "test prompt".into(),
    )
    .await;

    // Assert — turn succeeded despite the stale cancellation.
    assert!(
        result.is_ok(),
        "stale cancelled token should not cancel the new turn"
    );
}

#[tokio::test]
/// Verifies a clean post-turn tracked status completes without warning,
/// even when pre-turn status was dirty.
async fn test_run_channel_turn_skips_warning_when_main_checkout_is_clean_after_turn() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_in_progress_test_session(&db).await;

    let mut mock_channel = MockAgentChannel::new();
    mock_channel
        .expect_run_turn()
        .once()
        .returning(|_session_id, _req, _events| {
            Box::pin(async {
                Ok(TurnResult {
                    assistant_message: AgentResponse {
                        answer: "done".to_string(),
                        questions: Vec::new(),
                        review_comment_outcomes: Vec::new(),
                        subtasks: Vec::new(),
                        verification_verdicts: Vec::new(),
                    },
                    context_reset: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    provider_conversation_id: None,
                })
            })
        });

    let status_call_count = Arc::new(Mutex::new(0));
    let mut mock_git_client = mock_git_client_detecting_main_repo(base_dir.path().join("main"));
    mock_git_client
        .expect_tracked_worktree_status()
        .times(2)
        .returning(move |_| {
            let status_call_count = Arc::clone(&status_call_count);

            Box::pin(async move {
                let mut call_count = status_call_count
                    .lock()
                    .expect("status call count lock poisoned");
                *call_count += 1;
                if *call_count == 1 {
                    Ok(" M README.md\n".to_string())
                } else {
                    Ok(String::new())
                }
            })
        });
    mock_git_client.expect_head_hash().times(0);
    mock_git_client
        .expect_diff()
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    mock_git_client
        .expect_is_worktree_clean()
        .returning(|_| Box::pin(async { Ok(true) }));

    let transcript = empty_transcript();
    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(mock_channel),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: base_dir.path().to_path_buf(),
        fs_client: Arc::new(mock_fs_client_with_existing_directories()),
        git_client: Arc::new(mock_git_client),
        transcript: Arc::clone(&transcript),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        status: Arc::new(Mutex::new(Status::InProgress)),
    };

    // Act
    let result = run_channel_turn(
        &context,
        auto_commit_one_shot_client(),
        default_turn_metadata(),
        AgentRequestKind::SessionStart,
        None,
        "test prompt".into(),
    )
    .await;

    // Assert
    assert!(
        result.is_ok(),
        "clean post-turn tracked status should complete"
    );
    let output_text = transcript_text(&transcript);
    assert!(!output_text.contains("[Main Checkout Warning]"));
    assert!(output_text.contains("done"));
}

#[tokio::test]
/// Verifies an unchanged pre-existing dirty tracked status completes
/// without warning.
async fn test_run_channel_turn_skips_warning_when_main_checkout_stays_dirty() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_in_progress_test_session(&db).await;

    let mut mock_channel = MockAgentChannel::new();
    mock_channel
        .expect_run_turn()
        .once()
        .returning(|_session_id, _req, _events| {
            Box::pin(async {
                Ok(TurnResult {
                    assistant_message: AgentResponse {
                        answer: "done".to_string(),
                        questions: Vec::new(),
                        review_comment_outcomes: Vec::new(),
                        subtasks: Vec::new(),
                        verification_verdicts: Vec::new(),
                    },
                    context_reset: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    provider_conversation_id: None,
                })
            })
        });

    let mut mock_git_client = mock_git_client_detecting_main_repo(base_dir.path().join("main"));
    mock_git_client
        .expect_tracked_worktree_status()
        .times(2)
        .returning(|_| Box::pin(async { Ok(" M README.md\n".to_string()) }));
    mock_git_client.expect_head_hash().times(0);
    mock_git_client
        .expect_diff()
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    mock_git_client
        .expect_is_worktree_clean()
        .returning(|_| Box::pin(async { Ok(true) }));

    let transcript = empty_transcript();
    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(mock_channel),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: base_dir.path().to_path_buf(),
        fs_client: Arc::new(mock_fs_client_with_existing_directories()),
        git_client: Arc::new(mock_git_client),
        transcript: Arc::clone(&transcript),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        status: Arc::new(Mutex::new(Status::InProgress)),
    };

    // Act
    let result = run_channel_turn(
        &context,
        auto_commit_one_shot_client(),
        default_turn_metadata(),
        AgentRequestKind::SessionStart,
        None,
        "test prompt".into(),
    )
    .await;

    // Assert
    assert!(
        result.is_ok(),
        "unchanged dirty tracked status should complete"
    );
    let output_text = transcript_text(&transcript);
    assert!(!output_text.contains("[Main Checkout Warning]"));
    assert!(output_text.contains("done"));
}

#[tokio::test]
/// Verifies a bare shared repository (no main working checkout) skips the
/// main-checkout status snapshot: `tracked_worktree_status` is never called
/// and the turn proceeds with `main_checkout_root` set to `None`.
async fn test_run_channel_turn_skips_main_checkout_snapshot_for_bare_repo() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_in_progress_test_session(&db).await;

    let mut mock_channel = MockAgentChannel::new();
    mock_channel
        .expect_run_turn()
        .once()
        .withf(|_session_id, request, _events| request.main_checkout_root.is_none())
        .returning(|_session_id, _req, _events| {
            Box::pin(async {
                Ok(TurnResult {
                    assistant_message: AgentResponse {
                        answer: "done".to_string(),
                        questions: Vec::new(),
                        review_comment_outcomes: Vec::new(),
                        subtasks: Vec::new(),
                        verification_verdicts: Vec::new(),
                    },
                    context_reset: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    provider_conversation_id: None,
                })
            })
        });

    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
    mock_git_client
        .expect_main_checkout_working_tree()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client.expect_tracked_worktree_status().times(0);
    mock_git_client
        .expect_diff()
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    mock_git_client
        .expect_is_worktree_clean()
        .returning(|_| Box::pin(async { Ok(true) }));

    let transcript = empty_transcript();
    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(mock_channel),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: base_dir.path().to_path_buf(),
        fs_client: Arc::new(mock_fs_client_with_existing_directories()),
        git_client: Arc::new(mock_git_client),
        transcript: Arc::clone(&transcript),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        status: Arc::new(Mutex::new(Status::InProgress)),
    };

    // Act
    let result = run_channel_turn(
        &context,
        auto_commit_one_shot_client(),
        default_turn_metadata(),
        AgentRequestKind::SessionStart,
        None,
        "test prompt".into(),
    )
    .await;

    // Assert
    assert!(
        result.is_ok(),
        "bare shared repository turn should complete without a main-checkout snapshot"
    );
    let output_text = transcript_text(&transcript);
    assert!(!output_text.contains("[Main Checkout Warning]"));
    assert!(output_text.contains("done"));
}

#[tokio::test]
/// Verifies that a cancel arriving during the pre-turn setup window
/// (between the token swap in `run_channel_turn` and the entry into
/// `run_turn_with_cancellation`) is honoured immediately. The token is
/// already cancelled before `run_turn_with_cancellation` starts, so
/// `run_turn` must never be called.
async fn test_run_turn_with_cancellation_honours_pre_turn_cancel() {
    // Arrange — create a pre-cancelled token, simulating a Ctrl+c
    // that arrived during pre-turn setup.
    let mut unrelated_child = tokio::process::Command::new("sleep")
        .arg("60")
        .kill_on_drop(true)
        .spawn()
        .expect("start unrelated process");
    let recycled_pid = unrelated_child.id().expect("unrelated PID");
    let cancel_token = CancellationToken::new();
    cancel_token.cancel();

    let mut mock_channel = MockAgentChannel::new();
    // `run_turn` must NOT be called — the early-exit path returns
    // before reaching the select.
    mock_channel.expect_run_turn().never();
    mock_channel
        .expect_shutdown_session()
        .times(2)
        .returning(|_| Box::pin(async { Ok(()) }));

    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(mock_channel),
        child_pid: Arc::new(Mutex::new(Some(recycled_pid))),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: AppRepositories::in_memory().await.expect("db should open"),
        folder: std::env::temp_dir(),
        fs_client: Arc::new(fs::MockFsClient::new()),
        git_client: Arc::new(MockGitClient::new()),
        transcript: empty_transcript(),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

        session_update_versions: Arc::default(),
        session_id: "sess-preturn".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        status: Arc::new(Mutex::new(Status::InProgress)),
    };

    let req = TurnRequest {
        continuation: TurnContinuation::fresh(),
        folder: context.folder.clone(),
        main_checkout_root: None,
        model: "gemini-3.8-flash".to_string(),
        permission_mode: ag_agent::PermissionMode::AutoEdit,
        personality: ag_agent::PersonalityPrompt::default(),
        prompt: "test".into(),
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::SessionStart,
        response_style: ag_agent::ResponseStyle::default(),
        speed_mode: crate::domain::agent::SpeedMode::default(),
    };

    // Act — pass the pre-cancelled token directly.
    let result = run_turn_with_cancellation(
        &context,
        cancel_token.clone(),
        req.clone(),
        mpsc::unbounded_channel().0,
    )
    .await;

    *context.child_pid.lock().expect("PID slot") = Some(recycled_pid);
    let assist = SessionWorkerRebaseAssistClient::from_context(&context, None);
    let assist_result = assist
        .run_turn_with_cancellation(cancel_token, req, mpsc::unbounded_channel().0)
        .await;

    // Assert — a retained runtime's recycled PID never authorizes a signal.
    assert!(
        assist_result
            .expect_err("assist canceled")
            .to_string()
            .contains("[Stopped]")
    );
    assert!(context.child_pid.lock().expect("PID slot").is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), unrelated_child.wait())
            .await
            .is_err()
    );
    unrelated_child
        .kill()
        .await
        .expect("clean up owned process");
    let error_message = result.expect_err("should return an error").to_string();
    assert!(
        error_message.contains("[Stopped]"),
        "error should contain [Stopped], got: {error_message}"
    );
}

#[tokio::test]
/// Verifies that `run_turn_with_cancellation` returns `[Stopped]` even
/// when `run_turn` does not resolve after `shutdown_session`. The
/// 5-second timeout guard ensures the cancellation branch does not
/// block indefinitely.
async fn test_run_turn_with_cancellation_returns_stopped_after_drain_timeout() {
    for rebase_assist in [false, true] {
        // Arrange — mock channel whose `run_turn` never resolves and
        // whose `shutdown_session` completes immediately (simulating a
        // channel that ignores the shutdown request).
        let mut unrelated_child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .expect("start unrelated process");
        let recycled_pid = unrelated_child.id().expect("unrelated PID");
        let cancel_token = CancellationToken::new();

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .returning(|_session_id, _req, _events| {
                Box::pin(async {
                    // Never resolves — simulates a stuck channel.
                    std::future::pending::<Result<TurnResult, AgentError>>().await
                })
            });
        mock_channel
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(Some(recycled_pid))),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: AppRepositories::in_memory().await.expect("db should open"),
            folder: std::env::temp_dir(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess-timeout".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        let req = TurnRequest {
            continuation: TurnContinuation::fresh(),
            folder: context.folder.clone(),
            main_checkout_root: None,
            model: "gemini-3.8-flash".to_string(),
            permission_mode: ag_agent::PermissionMode::AutoEdit,
            personality: ag_agent::PersonalityPrompt::default(),
            prompt: "test".into(),
            reasoning_level: ReasoningLevel::default(),
            request_kind: AgentRequestKind::SessionStart,
            response_style: ag_agent::ResponseStyle::default(),
            speed_mode: crate::domain::agent::SpeedMode::default(),
        };

        // Spawn a task that cancels the token after a small delay so the
        // select branch fires mid-turn (not before the pre-check).
        let token_for_cancel = cancel_token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            token_for_cancel.cancel();
        });

        // Act — the drain timeout (5 seconds) runs with real wall-clock
        // delay. This test validates that the function does not block
        // indefinitely when `run_turn` never resolves.
        let result = if rebase_assist {
            SessionWorkerRebaseAssistClient::from_context(&context, None)
                .run_turn_with_cancellation(cancel_token, req, mpsc::unbounded_channel().0)
                .await
        } else {
            run_turn_with_cancellation(&context, cancel_token, req, mpsc::unbounded_channel().0)
                .await
        };

        // Assert — cancellation cannot signal a retained runtime's recycled
        // PID.
        assert!(
            unrelated_child
                .try_wait()
                .expect("poll unrelated process")
                .is_none()
        );
        unrelated_child
            .kill()
            .await
            .expect("clean up owned process");
        let error_message = result.expect_err("should return an error").to_string();
        assert!(
            error_message.contains("[Stopped]"),
            "error should contain [Stopped], got: {error_message}"
        );
    }
}

#[tokio::test]
/// Verifies an existing worktree with invalid metadata remains retryable.
async fn test_restart_recovery_preserves_git_inspection_failure() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    seed_recovery_test_operation(&db, Status::Rebasing, REBASE_OPERATION_KIND).await;
    let mut git_client = MockGitClient::new();
    git_client
        .expect_is_rebase_in_progress()
        .once()
        .returning(|_| {
            Box::pin(async { Err(ag_git::GitError::OutputParse("invalid gitdir".to_string())) })
        });
    git_client.expect_abort_rebase().times(0);

    // Act
    let result = SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
        &db,
        base_dir.path(),
        Arc::new(git_client),
        300,
    )
    .await;
    let sessions = db
        .sessions()
        .load_sessions()
        .await
        .expect("sessions should load");
    let unfinished = db
        .operations()
        .load_unfinished_session_operations()
        .await
        .expect("operations should load");

    // Assert
    assert!(matches!(result, Err(SessionError::Git(_))));
    assert_eq!(sessions[0].status, "Rebasing");
    assert_eq!(unfinished.len(), 1);
}

#[tokio::test]
/// Verifies recovery stops before interrupting operations when session
/// status reconciliation fails.
async fn test_fail_unfinished_operations_from_previous_run_returns_session_reconciliation_error() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    seed_recovery_test_operation(&db, Status::InProgress, "reply").await;
    sqlx::query(
        "CREATE TRIGGER fail_recovery_session_status BEFORE UPDATE OF status ON session BEGIN \
         SELECT RAISE(FAIL, 'session status failed'); END",
    )
    .execute(&pool)
    .await
    .expect("failed to create session status trigger");
    let mut mock_git_client = MockGitClient::new();
    mock_git_client.expect_is_rebase_in_progress().times(0);
    mock_git_client.expect_abort_rebase().times(0);

    // Act
    let result = SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
        &db,
        base_dir.path(),
        Arc::new(mock_git_client),
        300,
    )
    .await;
    let operation_is_unfinished = db
        .operations()
        .is_session_operation_unfinished("op-1")
        .await
        .expect("failed to check operation status");

    // Assert
    assert!(matches!(result, Err(SessionError::Db(_))));
    assert!(operation_is_unfinished);
}

#[tokio::test]
/// Verifies a later startup successfully retries recovery after an
/// earlier operation-interruption failure.
async fn test_fail_unfinished_operations_from_previous_run_retries_after_failure() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    seed_recovery_test_operation(&db, Status::InProgress, "reply").await;
    sqlx::query(
        "CREATE TRIGGER fail_recovery_retry BEFORE UPDATE OF status ON session_operation BEGIN \
         SELECT RAISE(FAIL, 'operation update failed'); END",
    )
    .execute(&pool)
    .await
    .expect("failed to create retry trigger");
    let mut failed_recovery_git_client = MockGitClient::new();
    failed_recovery_git_client
        .expect_is_rebase_in_progress()
        .times(0);
    failed_recovery_git_client.expect_abort_rebase().times(0);

    // Act
    let failed_recovery = SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
        &db,
        base_dir.path(),
        Arc::new(failed_recovery_git_client),
        300,
    )
    .await;
    sqlx::query("DROP TRIGGER fail_recovery_retry")
        .execute(&pool)
        .await
        .expect("failed to remove retry trigger");
    let mut successful_recovery_git_client = MockGitClient::new();
    successful_recovery_git_client
        .expect_is_rebase_in_progress()
        .times(0);
    successful_recovery_git_client
        .expect_abort_rebase()
        .times(0);
    let successful_recovery =
        SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
            &db,
            base_dir.path(),
            Arc::new(successful_recovery_git_client),
            301,
        )
        .await;
    let operation_is_unfinished = db
        .operations()
        .is_session_operation_unfinished("op-1")
        .await
        .expect("failed to check operation status");

    // Assert
    assert!(matches!(failed_recovery, Err(SessionError::Db(_))));
    assert!(successful_recovery.is_ok());
    assert!(!operation_is_unfinished);
}

#[tokio::test]
async fn test_worker_waits_for_foreground_gate_and_skips_abandoned_command() {
    // Arrange
    let (mut context, db, _queue, _directory) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::Draft).await;
    db.operations()
        .insert_session_operation("abandoned", "sess1", "reply")
        .await
        .expect("operation");
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    context.app_event_tx = event_tx;
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = oneshot::channel();
    let mut abandoned = ScheduledSessionCommand::immediate(resume_command("abandoned"));
    abandoned.ready_rx = Some(ready_rx);
    let reservation = Arc::new(());
    let reservation_observer = Arc::downgrade(&reservation);
    abandoned.preparation_reservation = Some(reservation);
    command_tx.send(abandoned).expect("gated command");
    command_tx
        .send(ScheduledSessionCommand::immediate(SessionCommand::Rebase {
            base_branch: "main".to_string(),
            operation_id: "already-resolved-rebase".to_string(),
        }))
        .expect("following command");
    SessionWorkerService::spawn_session_worker(
        context,
        auto_commit_one_shot_client(),
        Arc::default(),
        command_rx,
    );

    // Act
    assert!(
        tokio::time::timeout(Duration::from_millis(20), event_rx.recv())
            .await
            .is_err()
    );
    drop(ready_tx);
    let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
        .await
        .expect("worker resumed");

    // Assert
    assert!(matches!(
        event,
        Some(AppEvent::SessionQueuedSyncResolved { .. })
    ));
    assert!(event_rx.try_recv().is_err());
    assert_eq!(reservation_observer.strong_count(), 0);
    assert!(
        db.operations()
            .is_session_operation_unfinished("abandoned")
            .await
            .expect("unexecuted operation")
    );
}

#[test]
/// Ensures session command request kinds map to stable persisted
/// operation labels.
fn test_session_command_kind_values() {
    // Arrange
    let review_request_command = SessionCommand::CreateReviewRequest {
        branch_publish_session: BranchPublishTaskSession {
            base_branch: "main".to_string(),
            folder: PathBuf::new(),
            id: "sess1".into(),
            published_upstream_ref: None,
            review_request: None,
            status: Status::Review,
        },
        operation_id: "op-review-request".to_string(),
        remote_branch_name: None,
        response: None,
    };
    let start_command = SessionCommand::Run {
        operation_id: "op-start".to_string(),
        request_kind: AgentRequestKind::SessionStart,
        replay_transcript: None,
        prompt: "prompt".into(),
        turn_metadata: TurnMetadata {
            published_upstream_ref: None,
            review_comment_thread_ids: Vec::new(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Claude,
                AgentModel::ClaudeSonnet5,
            ),
        },
    };
    let resume_command = SessionCommand::Run {
        operation_id: "op-resume".to_string(),
        request_kind: AgentRequestKind::SessionResume,
        replay_transcript: None,
        prompt: "prompt".into(),
        turn_metadata: TurnMetadata {
            published_upstream_ref: None,
            review_comment_thread_ids: Vec::new(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Claude,
                AgentModel::ClaudeSonnet5,
            ),
        },
    };
    let account_read_command = SessionCommand::Run {
        operation_id: "op-account-read".to_string(),
        request_kind: AgentRequestKind::AccountRead,
        replay_transcript: None,
        prompt: "prompt".into(),
        turn_metadata: TurnMetadata {
            published_upstream_ref: None,
            review_comment_thread_ids: Vec::new(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Claude,
                AgentModel::ClaudeSonnet5,
            ),
        },
    };
    let focused_review_command = SessionCommand::Run {
        operation_id: "op-focused-review".to_string(),
        request_kind: AgentRequestKind::FocusedReview,
        replay_transcript: None,
        prompt: "prompt".into(),
        turn_metadata: TurnMetadata {
            published_upstream_ref: None,
            review_comment_thread_ids: Vec::new(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Claude,
                AgentModel::ClaudeSonnet5,
            ),
        },
    };

    // Act
    let review_request_kind = review_request_command.kind();
    let start_kind = start_command.kind();
    let resume_kind = resume_command.kind();
    let account_read_kind = account_read_command.kind();
    let focused_review_kind = focused_review_command.kind();

    // Assert
    assert_eq!(review_request_kind, "create_review_request");
    assert_eq!(start_kind, "start_prompt");
    assert_eq!(resume_kind, "reply");
    assert_eq!(account_read_kind, "account_read");
    assert_eq!(focused_review_kind, "focused_review");
}

#[tokio::test]
async fn test_next_scheduled_work_follows_shared_submission_order() {
    // Arrange
    let queued = VecDeque::from([
        queued_message(0, "queued first"),
        queued_message(1, "queued second"),
    ]);
    let (context, _db, queue_handle, _base_dir) =
        queue_test_context(MockAgentChannel::new(), queued, Status::InProgress).await;
    let mut pending_commands = VecDeque::from([ScheduledSessionCommand::queued(
        SessionCommand::Rebase {
            base_branch: "main".to_string(),
            operation_id: "queued-rebase".to_string(),
        },
        2,
    )]);

    // Act
    let first_work = SessionWorkerService::next_scheduled_work(&context, &mut pending_commands);
    let second_work = SessionWorkerService::next_scheduled_work(&context, &mut pending_commands);
    let third_work = SessionWorkerService::next_scheduled_work(&context, &mut pending_commands);
    queue_handle
        .lock()
        .expect("queue lock")
        .push_back(queued_message(4, "queued last"));
    pending_commands.push_back(ScheduledSessionCommand::queued(
        SessionCommand::Rebase {
            base_branch: "main".to_string(),
            operation_id: "older-rebase".to_string(),
        },
        3,
    ));
    let fourth_work = SessionWorkerService::next_scheduled_work(&context, &mut pending_commands);
    pending_commands.push_front(ScheduledSessionCommand::immediate(resume_command(
        "immediate-reply",
    )));
    let fifth_work = SessionWorkerService::next_scheduled_work(&context, &mut pending_commands);
    let sixth_work = SessionWorkerService::next_scheduled_work(&context, &mut pending_commands);

    // Assert
    assert!(matches!(
        first_work,
        Some(ScheduledSessionWork::Message(message))
            if message.transcript_text() == "queued first"
    ));
    assert!(matches!(
        second_work,
        Some(ScheduledSessionWork::Message(message))
            if message.transcript_text() == "queued second"
    ));
    assert!(matches!(
        third_work,
        Some(ScheduledSessionWork::Command(command))
            if command.queued_order == Some(2)
                && matches!(
                    &command.command,
                    SessionCommand::Rebase { operation_id, .. }
                        if operation_id == "queued-rebase"
                )
    ));
    assert!(matches!(
        fourth_work,
        Some(ScheduledSessionWork::Command(command))
            if command.queued_order == Some(3)
                && matches!(
                    &command.command,
                    SessionCommand::Rebase { operation_id, .. }
                        if operation_id == "older-rebase"
                )
    ));
    assert!(matches!(
        fifth_work,
        Some(ScheduledSessionWork::Command(command))
            if command.queued_order.is_none()
                && matches!(
                    &command.command,
                    SessionCommand::Run { operation_id, .. }
                        if operation_id == "immediate-reply"
                )
    ));
    assert!(matches!(
        sixth_work,
        Some(ScheduledSessionWork::Message(message))
            if message.transcript_text() == "queued last"
    ));
    assert!(queue_handle.lock().expect("queue lock").is_empty());
}

#[tokio::test]
async fn test_resolve_turn_personality_reports_unavailable_profile_once_and_clears_prior() {
    // Arrange
    let (mut context, db, _queue, _base_dir) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
    db.sessions()
        .update_session_personality_id("sess1", Some("missing".to_string()))
        .await
        .expect("personality selection should persist");
    persist_test_personality_state(
        &db,
        TurnPersonalityPersistence {
            applied_personality_id: Some("reviewer".to_string()),
            applied_personality_prompt_hash: Some("prior-hash".to_string()),
        },
    )
    .await;
    let mut personality_catalog_client = MockPersonalityCatalogClient::new();
    personality_catalog_client
        .expect_resolve()
        .times(2)
        .returning(|_, _| Box::pin(async { None }));
    context.personality_catalog_client = Arc::new(personality_catalog_client);

    // Act
    let first_resolution = resolve_turn_personality(&context).await;
    persist_test_personality_state(&db, first_resolution.persistence.clone()).await;
    let second_resolution = resolve_turn_personality(&context).await;
    let transcript = transcript_text(&context.transcript);

    // Assert
    assert_eq!(
        first_resolution.prompt,
        ag_agent::PersonalityPrompt::cleared(true)
    );
    assert_eq!(
        second_resolution.prompt,
        ag_agent::PersonalityPrompt::cleared(false)
    );
    assert_eq!(
        second_resolution.persistence,
        TurnPersonalityPersistence {
            applied_personality_id: Some("missing".to_string()),
            applied_personality_prompt_hash: None,
        }
    );
    assert_eq!(transcript.matches("is unavailable").count(), 1);
}

#[tokio::test]
async fn test_resolve_turn_personality_defaults_when_session_state_is_unavailable() {
    // Arrange
    let (mut context, _db, _queue, _base_dir) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
    context.session_id = "missing-session".into();

    // Act
    let missing_session = resolve_turn_personality(&context).await;
    let (closed_db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    pool.close().await;
    context.db = closed_db;
    let query_failure = resolve_turn_personality(&context)
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;

    // Assert
    assert_eq!(
        missing_session.prompt,
        ag_agent::PersonalityPrompt::default()
    );
    assert_eq!(
        missing_session.persistence,
        TurnPersonalityPersistence::default()
    );
    assert_eq!(query_failure.prompt, ag_agent::PersonalityPrompt::default());
    assert_eq!(
        query_failure.persistence,
        TurnPersonalityPersistence::default()
    );
}

#[tokio::test]
/// Verifies that `terminate_child_process` sends `SIGTERM` to the
/// child process tracked in the context's PID slot, killing it.
async fn test_terminate_child_process_sends_sigterm_to_active_child() {
    // Arrange — spawn a long-running child and store its PID in the
    // context.
    let mut child = tokio::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("failed to spawn sleep");
    let child_pid = child.id().expect("child has no pid");

    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(MockAgentChannel::new()),
        child_pid: Arc::new(Mutex::new(Some(child_pid))),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: AppRepositories::in_memory().await.expect("db should open"),
        folder: std::env::temp_dir(),
        fs_client: Arc::new(fs::MockFsClient::new()),
        git_client: Arc::new(MockGitClient::new()),
        transcript: empty_transcript(),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

        session_update_versions: Arc::default(),
        session_id: "sess-term".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Claude,
            AgentModel::ClaudeHaiku4520251001,
        ),
        status: Arc::new(Mutex::new(Status::InProgress)),
    };

    // Act
    terminate_child_process(&context.child_pid, context.session_agent.kind());

    // Assert — the child should have been terminated by SIGTERM.
    let exit_status = child.wait().await.expect("failed to wait on child");
    assert!(
        !exit_status.success(),
        "child should have been killed by SIGTERM"
    );
    // PID slot should be cleared after termination.
    assert!(
        context.child_pid.lock().expect("child_pid lock").is_none(),
        "PID slot should be cleared after termination"
    );
}

#[tokio::test]
/// Verifies that `terminate_child_process` is a no-op when no child
/// PID is stored for a CLI channel.
async fn test_terminate_child_process_noop_when_no_pid() {
    // Arrange
    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(MockAgentChannel::new()),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: AppRepositories::in_memory().await.expect("db should open"),
        folder: std::env::temp_dir(),
        fs_client: Arc::new(fs::MockFsClient::new()),
        git_client: Arc::new(MockGitClient::new()),
        transcript: empty_transcript(),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

        session_update_versions: Arc::default(),
        session_id: "sess-nopid".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Claude,
            AgentModel::ClaudeHaiku4520251001,
        ),
        status: Arc::new(Mutex::new(Status::InProgress)),
    };

    // Act — should not panic or error.
    terminate_child_process(&context.child_pid, context.session_agent.kind());

    // Assert — PID slot remains None.
    assert!(
        context.child_pid.lock().expect("child_pid lock").is_none(),
        "PID slot should still be None"
    );
}
