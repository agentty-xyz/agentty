use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use ag_agent as agent;
use ag_agent::MockOneShotClient;
use ag_git::{GitError, MockGitClient};
use tokio::sync::mpsc;

use super::super::{
    RunAgentAssistTaskInput, SessionTaskService, SessionTranscriptMessageAppend, StatusTransition,
};
use super::support::{StaticClock, insert_review_session, one_shot_submission};
use crate::app::AppEvent;
use crate::app::assist::AssistContext;
use crate::app::service::{AppServiceDeps, AppServices};
use crate::db::AppRepositories;
use crate::domain::agent::{AgentCliInfo, AgentKind, AgentModel, AgentSelection};
use crate::domain::session::{SessionHandles, Status};
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::infra::fs;

#[tokio::test]
async fn test_append_workflow_notice_updates_live_and_durable_workflow_transcript() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::Gpt56Sol.as_str()).await;
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
    let session_update_versions = Arc::default();

    // Act
    SessionTaskService::append_workflow_notice(
        &transcript,
        &database,
        &app_event_tx,
        &session_update_versions,
        "session-id",
        "\n[Commit] No changes to commit.\n",
    )
    .await;

    // Assert
    assert_eq!(
        transcript
            .lock()
            .expect("transcript lock should not be poisoned")
            .replay_text()
            .expect("transcript should have replay text"),
        "\n[Commit] No changes to commit.\n"
    );
    assert_eq!(
        transcript
            .lock()
            .expect("transcript lock should not be poisoned")
            .messages(),
        &[SessionMessage::new(
            0,
            SessionMessageKind::WorkflowNotice,
            "\n[Commit] No changes to commit.\n"
        )]
    );
    let messages = database
        .sessions()
        .load_session_messages("session-id")
        .await
        .expect("failed to load persisted session messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, "workflow_notice");
    assert_eq!(messages[0].content, "\n[Commit] No changes to commit.\n");
}

#[tokio::test]
/// Verifies successful auto-commit updates the persisted session title.
async fn test_handle_auto_commit_updates_session_title() {
    // Arrange
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_check_pre_commit_hook_ready()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok::<_, GitError>(false) }));
    mock_git_client
        .expect_diff()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok("diff --git a/a.rs b/a.rs".to_string()) }));
    mock_git_client
        .expect_has_commits_since()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok::<_, GitError>(true) }));
    mock_git_client
        .expect_head_commit_message()
        .times(1)
        .returning(|_| {
            Box::pin(async {
                Ok::<_, GitError>(Some(
                    "Refine README updates\n\n- Keep title aligned with commit".to_string(),
                ))
            })
        });
    mock_git_client
        .expect_commit_all_preserving_single_commit()
        .times(1)
        .returning(|_, _, _, _| Box::pin(async { Ok::<_, GitError>(()) }));
    mock_git_client
        .expect_head_short_hash()
        .times(1)
        .returning(|_| Box::pin(async { Ok::<_, GitError>("abc1234".to_string()) }));
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::Gpt56Sol.as_str()).await;
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client.expect_submit().times(1).returning(|_| {
        Ok(one_shot_submission(
            "Refine README updates\n\n- Keep title aligned with commit",
            0,
            0,
        ))
    });
    let context = AssistContext {
        app_event_tx,
        child_pid: Arc::new(Mutex::new(None)),
        db: database.clone(),
        folder: PathBuf::from("/tmp/project"),
        git_client: Arc::new(mock_git_client),
        id: "session-id".to_string(),
        one_shot_client: Arc::new(one_shot_client),
        session_agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
        session_update_versions: Arc::default(),
        transcript: Arc::clone(&transcript),
    };

    // Act
    SessionTaskService::handle_auto_commit(context).await;
    let sessions = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");

    // Assert
    assert_eq!(sessions[0].title.as_deref(), Some("Refine README updates"));
    let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();
    let output_text = transcript
        .lock()
        .ok()
        .and_then(|buffer| buffer.replay_text())
        .unwrap_or_default();
    assert!(!output_text.contains("[Commit] committed with hash `abc1234`"));
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::SessionWorkflowNoticeUpdated {
            notice,
            session_id,
        } if session_id.as_str() == "session-id"
            && notice == "[Commit] committed with hash `abc1234`"
    )));
    assert!(events.contains(&AppEvent::RefreshGitStatus));
}

/// Verifies the status-transition context updates both live handles and
/// persisted rows when built from shared services.
#[tokio::test]
async fn test_status_transition_from_services_updates_handle_and_persistence() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, "gpt-5.6-sol").await;
    let handles = SessionHandles::new(Status::Review);
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let services = AppServices::new_with_agent_clis(
        PathBuf::from("/tmp/agentty-tests"),
        Arc::new(StaticClock::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(42),
        )),
        app_event_tx,
        AppServiceDeps {
            app_server_client_override: Some(crate::test_support::mock_app_server()),
            available_agent_kinds: AgentKind::ALL.to_vec(),
            clipboard_image_client_override: None,
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            one_shot_client_override: None,
            personality_catalog_client_override: None,
            repositories: database.clone(),
            review_request_client: Arc::new(ag_forge::MockReviewRequestClient::new()),
        },
        AgentCliInfo::from_kinds(AgentKind::ALL),
    );
    let status_transition = StatusTransition::from_services(&services, &handles, "session-id");

    // Act
    let status_updated = status_transition.apply(Status::InProgress).await;
    let live_status = handles
        .status
        .lock()
        .expect("status lock should not be poisoned")
        .to_string();
    let session_row = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions")
        .into_iter()
        .find(|row| row.id == "session-id")
        .expect("missing session row");

    // Assert
    assert!(status_updated);
    assert_eq!(live_status, Status::InProgress.to_string());
    assert_eq!(session_row.status, Status::InProgress.to_string());
}

#[tokio::test]
async fn test_append_session_transcript_message_updates_live_and_durable_typed_transcript() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::Gpt56Sol.as_str()).await;
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
    let session_update_versions = Arc::default();

    // Act
    SessionTaskService::append_session_transcript_message(
        &transcript,
        &database,
        &app_event_tx,
        &session_update_versions,
        "session-id",
        SessionTranscriptMessageAppend {
            kind: SessionMessageKind::UserPrompt,
            raw_content: "    hello ",
        },
    )
    .await;

    // Assert
    assert_eq!(
        transcript
            .lock()
            .expect("transcript lock should not be poisoned")
            .replay_text()
            .expect("transcript should have replay text"),
        " ›     hello\n\n"
    );
    assert_eq!(
        transcript
            .lock()
            .expect("transcript lock should not be poisoned")
            .messages(),
        &[SessionMessage::conversation(
            0,
            SessionMessageKind::UserPrompt,
            "    hello"
        )]
    );
    let messages = database
        .sessions()
        .load_session_messages("session-id")
        .await
        .expect("failed to load persisted session messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, "user_prompt");
    assert_eq!(messages[0].content, "    hello");
}

#[test]
/// Verifies lifecycle statuses that require full list refreshes are
/// enumerated correctly.
fn test_status_requires_full_refresh_for_lifecycle_statuses() {
    // Arrange
    let refresh_statuses = [
        Status::InProgress,
        Status::Review,
        Status::Merging,
        Status::Merged,
        Status::Done,
        Status::Canceled,
    ];

    // Act & Assert
    for status in refresh_statuses {
        assert!(SessionTaskService::status_requires_full_refresh(status));
    }
    assert!(!SessionTaskService::status_requires_full_refresh(
        Status::Draft
    ));
}

#[tokio::test]
/// Verifies non-zero assist subprocess exits surface the one-shot command
/// error details.
async fn test_run_agent_assist_task_returns_error_for_non_zero_exit_status() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::ClaudeOpus5.as_str()).await;
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client.expect_submit().returning(|_| {
        Err(agent::OneShotError::new(
            "One-shot agent command failed with exit code 7: assist failed",
        ))
    });

    // Act
    let result = SessionTaskService::run_agent_assist_task(RunAgentAssistTaskInput {
        app_event_tx,
        child_pid: Arc::new(Mutex::new(None)),
        db: database.clone(),
        folder: temp_dir.path().to_path_buf(),
        id: "session-id".to_string(),
        one_shot_client: Arc::new(one_shot_client),
        prompt: "Resolve conflict".to_string(),
        session_agent: AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus5),
        session_update_versions: Arc::default(),
        transcript: Arc::new(Mutex::new(SessionTranscript::default())),
    })
    .await;

    // Assert
    assert!(result.is_err());
    let error_text = result.expect_err("expected non-zero exit to fail");
    assert!(error_text.to_string().contains("exit code 7"));
    assert!(error_text.to_string().contains("assist failed"));
}

#[tokio::test]
async fn test_update_status_accumulates_repeated_in_progress_intervals() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session(
            "session-id",
            "gpt-5.6-sol",
            "main",
            &Status::Draft.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert session");
    let status = Mutex::new(Status::Draft);
    let clock = StaticClock::new(SystemTime::UNIX_EPOCH + Duration::from_secs(10));
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let session_update_versions = Arc::default();

    // Act
    let entered_first_interval = SessionTaskService::update_status(
        &status,
        &clock,
        &database,
        &app_event_tx,
        &session_update_versions,
        "session-id",
        Status::InProgress,
    )
    .await;
    clock.set_now_system_time(SystemTime::UNIX_EPOCH + Duration::from_secs(70));
    let left_first_interval = SessionTaskService::update_status(
        &status,
        &clock,
        &database,
        &app_event_tx,
        &session_update_versions,
        "session-id",
        Status::Review,
    )
    .await;
    clock.set_now_system_time(SystemTime::UNIX_EPOCH + Duration::from_secs(100));
    let entered_second_interval = SessionTaskService::update_status(
        &status,
        &clock,
        &database,
        &app_event_tx,
        &session_update_versions,
        "session-id",
        Status::InProgress,
    )
    .await;
    clock.set_now_system_time(SystemTime::UNIX_EPOCH + Duration::from_secs(190));
    let left_second_interval = SessionTaskService::update_status(
        &status,
        &clock,
        &database,
        &app_event_tx,
        &session_update_versions,
        "session-id",
        Status::Question,
    )
    .await;
    let session_row = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions")
        .into_iter()
        .find(|row| row.id == "session-id")
        .expect("missing session row");

    // Assert
    assert!(entered_first_interval);
    assert!(left_first_interval);
    assert!(entered_second_interval);
    assert!(left_second_interval);
    assert_eq!(session_row.status, "Question");
    assert_eq!(session_row.in_progress_started_at, None);
    assert_eq!(session_row.in_progress_total_seconds, 150);
}
