use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use ag_agent as agent;
use ag_agent::{AgentRequestKind, MockAgentChannel, TurnResult};
use ag_forge as forge;
use ag_git::MockGitClient;
use ag_protocol::AgentResponse;
use tempfile::tempdir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::super::super::turn::run_channel_turn;
use super::super::{
    ScheduledSessionCommand, SessionWorkerContext, SessionWorkerService, TurnMetadata,
};
use super::support::{
    apply_worker_turn_result, auto_commit_one_shot_client, default_turn_metadata, empty_transcript,
    insert_in_progress_test_session, mock_git_client_detecting_main_repo, queue_test_context,
    resume_command, transcript_text,
};
use crate::app::AppEvent;
use crate::app::session::SessionError;
use crate::domain::agent::{AgentModel, AgentSelection};
use crate::domain::session::{SessionId, Status};
use crate::infra::db::AppRepositories;
use crate::infra::fs;
use crate::infra::personality::RealPersonalityCatalogClient;

#[tokio::test]
/// Verifies failed turn-metadata persistence forces a refresh and skips
/// reducer projection emission.
async fn test_apply_turn_result_refreshes_when_turn_metadata_persistence_fails() {
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
    db.sessions()
        .delete_session("sess1")
        .await
        .expect("failed to delete session");
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let context = SessionWorkerContext {
        app_event_tx,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(MockAgentChannel::new()),
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
    let turn_result = Ok(TurnResult {
        assistant_message: AgentResponse {
            answer: "Implemented the change.".to_string(),
            questions: Vec::new(),
            review_comment_outcomes: Vec::new(),
            subtasks: Vec::new(),
            verification_verdicts: Vec::new(),
        },
        context_reset: false,
        input_tokens: 2,
        output_tokens: 3,
        provider_conversation_id: None,
    });

    // Act
    let turn_metadata = TurnMetadata {
        published_upstream_ref: None,
        review_comment_thread_ids: Vec::new(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
    };
    let error = apply_worker_turn_result(&context, turn_metadata, turn_result)
        .await
        .expect_err("turn result should fail when metadata persistence fails");
    let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();
    let output = transcript_text(&context.transcript);

    // Assert
    assert!(
        error
            .to_string()
            .contains("no rows returned by a query that expected to return at least one row")
    );
    assert!(output.contains("Implemented the change."));
    assert!(output.contains("[Turn Metadata Error] Failed to persist completed turn metadata:"));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AppEvent::RefreshSessions))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AppEvent::AgentResponseReceived { .. }))
    );
}

#[tokio::test]
/// Verifies turn persistence appends only the protocol answer to assistant
/// transcript messages.
async fn test_apply_turn_result_persists_only_assistant_answer() {
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

    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(true) }));
    let transcript = Arc::new(Mutex::new(crate::test_support::assistant_transcript(
        "Hey! How can I help you today?",
    )));
    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(MockAgentChannel::new()),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: base_dir.path().to_path_buf(),
        fs_client: Arc::new(fs::MockFsClient::new()),
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
    let turn_result = Ok(TurnResult {
        assistant_message: AgentResponse {
            answer: "Hey! How can I help you today?".to_string(),
            questions: Vec::new(),
            review_comment_outcomes: Vec::new(),
            subtasks: Vec::new(),
            verification_verdicts: Vec::new(),
        },
        context_reset: false,
        input_tokens: 0,
        output_tokens: 0,
        provider_conversation_id: None,
    });

    // Act
    let turn_metadata = TurnMetadata {
        published_upstream_ref: None,
        review_comment_thread_ids: Vec::new(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
    };
    let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
        .await
        .expect("turn result should succeed");
    let output = transcript_text(&transcript);

    // Assert
    assert_eq!(status, Status::Review);
    assert!(
        output.starts_with("Hey! How can I help you today?\n\nHey! How can I help you today?\n\n")
    );
    assert!(!output.contains("[Commit] No changes to commit."));
    assert!(!output.contains("## Change Summary"));
}

#[tokio::test]
/// Persists the current app-server instruction bootstrap marker after a
/// successful turn so later follow-ups can reuse the compact reminder.
async fn test_apply_turn_result_persists_instruction_conversation_id_for_app_server_turns() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("sess1", "gpt-5.6-sol", "main", "InProgress", project_id)
        .await
        .expect("failed to insert session");

    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(true) }));
    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(MockAgentChannel::new()),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: base_dir.path().to_path_buf(),
        fs_client: Arc::new(fs::MockFsClient::new()),
        git_client: Arc::new(mock_git_client),
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
    let turn_result = Ok(TurnResult {
        assistant_message: AgentResponse {
            answer: "Implemented the change.".to_string(),
            questions: Vec::new(),
            review_comment_outcomes: Vec::new(),
            subtasks: Vec::new(),
            verification_verdicts: Vec::new(),
        },
        context_reset: true,
        input_tokens: 0,
        output_tokens: 0,
        provider_conversation_id: Some("thread-123".to_string()),
    });

    // Act
    let turn_metadata = TurnMetadata {
        published_upstream_ref: None,
        review_comment_thread_ids: Vec::new(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Codex,
            AgentModel::Gpt56Sol,
        ),
    };
    let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
        .await
        .expect("turn result should succeed");
    let instruction_conversation_id = db
        .sessions()
        .get_session_instruction_conversation_id("sess1")
        .await
        .expect("failed to load instruction conversation id");

    // Assert
    assert_eq!(status, Status::Review);
    assert_eq!(
        instruction_conversation_id,
        agent::normalize_instruction_conversation_id(Some("thread-123"))
    );
}

#[tokio::test]
async fn test_run_channel_turn_persists_failure_when_main_checkout_snapshot_fails() {
    // Arrange
    let (mut context, _db, _queue, base_dir) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
    let main_repo_root = base_dir.path().join("main");
    let mut mock_git_client = mock_git_client_detecting_main_repo(main_repo_root);
    mock_git_client
        .expect_diff()
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    mock_git_client
        .expect_is_worktree_clean()
        .returning(|_| Box::pin(async { Ok(true) }));
    mock_git_client
        .expect_tracked_worktree_status()
        .once()
        .returning(|_| {
            Box::pin(async {
                Err(ag_git::GitError::CommandFailed {
                    command: "git status".to_string(),
                    stderr: "status failed".to_string(),
                })
            })
        });
    context.git_client = Arc::new(mock_git_client);

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
    assert!(matches!(result, Err(SessionError::Workflow(_))));
    assert!(transcript_text(&context.transcript).contains("status failed"));
}

#[tokio::test]
/// Verifies recovery stops immediately when unfinished operations cannot
/// be loaded from storage.
async fn test_fail_unfinished_operations_from_previous_run_returns_load_error() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    pool.close().await;
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

    // Assert
    assert!(matches!(result, Err(SessionError::Db(_))));
}

#[tokio::test]
async fn test_send_persisted_command_marks_failed_when_worker_receiver_is_closed() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_in_progress_test_session(&database).await;
    database
        .operations()
        .insert_session_operation("rollup-failed", "sess1", "reply")
        .await
        .expect("failed to insert operation");
    let (sender, receiver) = mpsc::unbounded_channel();
    drop(receiver);
    let mut worker_service = SessionWorkerService::new();

    // Act
    let result = worker_service
        .send_persisted_command(
            database.operations(),
            &SessionId::from("sess1"),
            sender,
            ScheduledSessionCommand::immediate(resume_command("rollup-failed")),
        )
        .await;
    let unfinished = database
        .operations()
        .is_session_operation_unfinished("rollup-failed")
        .await
        .expect("failed to inspect operation");

    // Assert
    assert!(matches!(
        result,
        Err(SessionError::Workflow(error))
            if error == "Session worker is not available"
    ));
    assert!(!unfinished);
}
