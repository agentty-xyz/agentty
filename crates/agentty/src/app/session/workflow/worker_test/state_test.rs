use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use ag_agent::{AgentRequestKind, MockAgentChannel, TurnEvent, TurnResult};
use ag_forge as forge;
use ag_git::MockGitClient;
use ag_protocol::AgentResponse;
use tempfile::tempdir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::super::super::post_turn::status_update_after_turn_result;
use super::super::super::turn::{consume_turn_events, run_channel_turn};
use super::super::{SessionWorkerContext, SessionWorkerService};
use super::support::{
    auto_commit_one_shot_client, default_turn_metadata, empty_transcript,
    insert_in_progress_test_session, mock_fs_client_with_existing_directories,
    mock_git_client_detecting_main_repo, seed_recovery_test_operation, transcript_text,
};
use crate::app::AppEvent;
use crate::app::session::SessionError;
use crate::domain::agent::{AgentModel, AgentSelection};
use crate::domain::session::Status;
use crate::infra::db::AppRepositories;
use crate::infra::personality::RealPersonalityCatalogClient;

#[tokio::test]
/// Verifies process-only events do not append transcript content.
async fn test_consume_turn_events_ignores_pid_only_events_for_transcript_messages() {
    // Arrange
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let child_pid = Arc::new(Mutex::new(None));

    event_tx
        .send(TurnEvent::PidUpdate(Some(4242)))
        .expect("failed to send pid update");
    drop(event_tx);

    // Act
    consume_turn_events(
        event_rx,
        app_event_tx,
        "session-1".into(),
        Arc::clone(&child_pid),
    )
    .await;

    // Assert
    assert_eq!(*child_pid.lock().expect("pid lock poisoned"), Some(4242));
    assert!(app_event_rx.try_recv().is_err());
}

#[tokio::test]
/// Verifies thought deltas update the loader state without appending
/// transcript messages.
async fn test_consume_turn_events_routes_thought_delta_to_progress_state_only() {
    // Arrange
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let child_pid = Arc::new(Mutex::new(None));

    event_tx
        .send(TurnEvent::ThoughtDelta("Inspecting files".to_string()))
        .expect("failed to send thought delta");
    drop(event_tx);

    // Act
    consume_turn_events(event_rx, app_event_tx, "session-1".into(), child_pid).await;

    let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();

    // Assert
    assert_eq!(
        events,
        vec![
            AppEvent::SessionProgressUpdated {
                progress_message: Some("Inspecting files".to_string()),
                session_id: "session-1".into(),
            },
            AppEvent::SessionProgressUpdated {
                progress_message: None,
                session_id: "session-1".into(),
            },
        ]
    );
}

#[tokio::test]
/// Verifies ready thought-delta bursts enqueue only the latest progress
/// update before the final clear event.
async fn test_consume_turn_events_coalesces_ready_thought_delta_bursts() {
    // Arrange
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let child_pid = Arc::new(Mutex::new(None));

    for thought in ["first", "second", "third"] {
        event_tx
            .send(TurnEvent::ThoughtDelta(thought.to_string()))
            .expect("failed to send thought delta");
    }
    drop(event_tx);

    // Act
    consume_turn_events(event_rx, app_event_tx, "session-1".into(), child_pid).await;

    let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();

    // Assert
    assert_eq!(
        events,
        vec![
            AppEvent::SessionProgressUpdated {
                progress_message: Some("third".to_string()),
                session_id: "session-1".into(),
            },
            AppEvent::SessionProgressUpdated {
                progress_message: None,
                session_id: "session-1".into(),
            },
        ]
    );
}

#[tokio::test]
/// Verifies a turn that dirties the main checkout records a warning while
/// preserving the successful agent response.
async fn test_run_channel_turn_warns_when_main_checkout_status_changes() {
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
                    Ok(String::new())
                } else {
                    Ok(" M README.md\n".to_string())
                }
            })
        });
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
    assert!(result.is_ok(), "main checkout changes should warn only");
    let output_text = transcript_text(&transcript);
    assert!(output_text.contains("[Main Checkout Warning]"));
    assert!(output_text.contains("tracked-file status changed"));
    assert!(output_text.contains("done"));
}

#[test]
fn test_status_update_after_turn_result_skips_stopped_by_user() {
    // Arrange
    let result = Err(SessionError::StoppedByUser(
        "[Stopped] Session interrupted by user.".to_string(),
    ));

    // Act
    let status_update = status_update_after_turn_result(&result);

    // Assert
    assert_eq!(status_update, None);
}

#[tokio::test]
/// Verifies recovery returns an operation-interruption failure after
/// session reconciliation rather than admitting normal work.
async fn test_fail_unfinished_operations_from_previous_run_returns_operation_update_error() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    seed_recovery_test_operation(&db, Status::InProgress, "reply").await;
    sqlx::query(
        "CREATE TRIGGER fail_recovery_operation_update BEFORE UPDATE OF status ON \
         session_operation BEGIN SELECT RAISE(FAIL, 'operation update failed'); END",
    )
    .execute(&pool)
    .await
    .expect("failed to create operation update trigger");
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
    let sessions = db
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    let operation_is_unfinished = db
        .operations()
        .is_session_operation_unfinished("op-1")
        .await
        .expect("failed to check operation status");

    // Assert
    assert!(matches!(result, Err(SessionError::Db(_))));
    assert_eq!(sessions[0].status, "Review");
    assert!(operation_is_unfinished);
}
