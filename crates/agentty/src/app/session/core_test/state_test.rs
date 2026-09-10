use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use ag_agent::{AppServerClient, AppServerTurnResponse, MockAgentBackend, MockAppServerClient};
use tempfile::tempdir;

use super::super::{SessionManager, session_folder};
use super::support::{
    new_test_app_with_db, new_test_app_with_db_and_app_server, new_test_app_with_git,
    test_session_manager, wait_for_status, wait_for_status_with_retries,
};
use crate::app::session::SessionLoadInput;
use crate::app::session::workflow::task::SessionTaskService;
use crate::domain::agent::{
    AgentKind, AgentModel, AgentSelection, ReasoningLevel, ResponseStyle, SpeedMode,
};
use crate::domain::permission::PermissionMode;
use crate::domain::session::{SESSION_DATA_DIR, SessionSize, Status};
use crate::domain::transient_message::{TransientMessageAnchor, TransientMessageSlot};
use crate::infra::clock::RealClock;
use crate::infra::db::AppRepositories;
use crate::infra::fs;

#[tokio::test]
/// Ensures transitioning a Codex session to `Done` shuts down its
/// app-server runtime.
async fn test_done_status_triggers_app_server_shutdown() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut mock_app_server = MockAppServerClient::new();
    mock_app_server
        .expect_run_turn()
        .times(1)
        .returning(|_, _| {
            Box::pin(async {
                Ok(AppServerTurnResponse {
                    assistant_message: r#"{"answer":"ready","questions":[]}"#.to_string(),
                    context_reset: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    pid: None,
                    provider_conversation_id: None,
                })
            })
        });
    mock_app_server
        .expect_shutdown_session()
        .times(1)
        .returning(move |session_id| {
            let shutdown_tx = shutdown_tx.clone();
            Box::pin(async move {
                let _ = shutdown_tx.send(session_id);
            })
        });
    let app_server_client: Arc<dyn AppServerClient> = Arc::new(mock_app_server);
    let mut app = new_test_app_with_db_and_app_server(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Some("main".to_string()),
        db,
        app_server_client,
    )
    .await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.set_session_model(
        &session_id,
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
    )
    .await
    .expect("failed to set app-server model");

    // Act
    app.sessions
        .reply(&app.services, &session_id, "Start")
        .await;
    wait_for_status(&mut app, &session_id, Status::Review).await;
    let handles = app
        .sessions
        .session_handles_or_err(&session_id)
        .expect("missing session handles");
    let session_status = Arc::clone(&handles.status);
    let app_event_tx = app.services.event_sender();
    let transitioned_to_merging = SessionTaskService::update_status(
        &session_status,
        app.services.clock().as_ref(),
        app.services.db(),
        &app_event_tx,
        &app.services.session_update_versions(),
        &session_id,
        Status::Merging,
    )
    .await;
    assert!(
        transitioned_to_merging,
        "status transition to Merging should succeed"
    );
    let transitioned_to_done = SessionTaskService::update_status(
        &session_status,
        app.services.clock().as_ref(),
        app.services.db(),
        &app_event_tx,
        &app.services.session_update_versions(),
        &session_id,
        Status::Done,
    )
    .await;
    assert!(
        transitioned_to_done,
        "status transition to Done should succeed"
    );
    app.process_pending_app_events().await;
    wait_for_status(&mut app, &session_id, Status::Done).await;
    let shutdown_session_id =
        tokio::time::timeout(std::time::Duration::from_secs(1), shutdown_rx.recv())
            .await
            .expect("timed out waiting for app-server shutdown")
            .expect("missing shutdown session id");

    // Assert
    assert_eq!(shutdown_session_id, session_id);
}

#[test]
fn test_append_workflow_notice_anchors_active_status_notices_after_active_turn() {
    for status in [Status::InProgress, Status::Queued] {
        // Arrange
        let mut session_manager = test_session_manager("session-id", None);
        session_manager.sessions_mut()[0].status = status;

        // Act
        session_manager.append_workflow_notice(
            "session-id",
            "[Sync] Queued until the current turn finishes.".to_string(),
        );

        // Assert
        let workflow_notice = session_manager.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::WorkflowNotice)
            .expect("workflow notice should be present");
        assert_eq!(
            workflow_notice.anchor,
            TransientMessageAnchor::AfterActiveTurn,
            "unexpected anchor for {status}"
        );
    }
}

#[tokio::test]
async fn test_load_sessions_uses_persisted_size_for_non_terminal_status() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.services
        .db()
        .sessions()
        .update_session_diff_stats(8, 3, true, &session_id, "S")
        .await
        .expect("failed to update size");
    let session_index = app
        .session_index_for_id(&session_id)
        .expect("missing created session");
    let session_folder = app.sessions.sessions()[session_index].folder.clone();
    let changed_lines = "line\n".repeat(700);
    std::fs::write(session_folder.join("size-test.txt"), changed_lines)
        .expect("failed to write test file");

    // Act
    let fs_client = fs::RealFsClient;
    let (reloaded_sessions, _, _) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: app.projects.active_project_id(),
            active_session_id: None,
            base: app.services.base_path(),
            clock: &RealClock,
            db: app.services.db(),
            fs_client: &fs_client,
            working_dir: app.projects.working_dir(),
        },
        app.sessions.session_handles_mut(),
    )
    .await;
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");

    // Assert
    let reloaded_session = reloaded_sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing reloaded session");
    let db_session = db_sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing persisted session");
    assert_eq!(reloaded_session.size, SessionSize::S);
    assert_eq!(reloaded_session.stats.added_lines, 8);
    assert_eq!(reloaded_session.stats.deleted_lines, 3);
    assert_eq!(db_session.added_lines, 8);
    assert_eq!(db_session.deleted_lines, 3);
    assert_eq!(db_session.size, "S");
}

#[tokio::test]
async fn test_load_sessions_uses_persisted_size_for_done_status() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.services
        .db()
        .sessions()
        .update_session_diff_stats(21, 9, true, &session_id, "L")
        .await
        .expect("failed to update size");
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&session_id, "Done", 0)
        .await
        .expect("failed to update status");
    let session_index = app
        .session_index_for_id(&session_id)
        .expect("missing created session");
    let session_folder = app.sessions.sessions()[session_index].folder.clone();
    let changed_lines = "line\n".repeat(700);
    std::fs::write(session_folder.join("done-size-test.txt"), changed_lines)
        .expect("failed to write test file");

    // Act
    let fs_client = fs::RealFsClient;
    let (reloaded_sessions, _, _) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: app.projects.active_project_id(),
            active_session_id: None,
            base: app.services.base_path(),
            clock: &RealClock,
            db: app.services.db(),
            fs_client: &fs_client,
            working_dir: app.projects.working_dir(),
        },
        app.sessions.session_handles_mut(),
    )
    .await;
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");

    // Assert
    let reloaded_session = reloaded_sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing reloaded session");
    let db_session = db_sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing persisted session");
    assert_eq!(reloaded_session.status, Status::Done);
    assert_eq!(reloaded_session.size, SessionSize::L);
    assert_eq!(reloaded_session.stats.added_lines, 21);
    assert_eq!(reloaded_session.stats.deleted_lines, 9);
    assert_eq!(db_session.added_lines, 21);
    assert_eq!(db_session.deleted_lines, 9);
    assert_eq!(db_session.size, "L");
}

#[tokio::test]
async fn test_cancel_session_requires_cancelable_status() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    // Status is New

    // Act
    let result = app
        .sessions
        .cancel_session(&app.services, &session_id)
        .await;

    // Assert
    assert!(result.is_err());
    assert!(
        result
            .expect_err("should be error")
            .to_string()
            .contains("not cancelable in its current state")
    );
}

#[tokio::test]
async fn test_load_existing_sessions_ordered_by_updated_at_desc() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("alpha000", "claude-opus-5", "main", "Done", project_id)
        .await
        .expect("failed to insert alpha000");
    db.sessions()
        .insert_session("beta0000", "gemini-3.8-flash", "main", "Done", project_id)
        .await
        .expect("failed to insert beta0000");

    db.sessions()
        .update_session_updated_at("alpha000", 1_i64)
        .await
        .expect("failed to update alpha000 timestamp");
    db.sessions()
        .update_session_updated_at("beta0000", 2_i64)
        .await
        .expect("failed to update beta0000 timestamp");

    for session_id in ["alpha000", "beta0000"] {
        let session_dir = session_folder(dir.path(), session_id);
        let data_dir = session_dir.join(SESSION_DATA_DIR);
        std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
    }

    // Act
    let app = new_test_app_with_db(
        dir.path().to_path_buf(),
        PathBuf::from("/tmp/test"),
        None,
        db,
    )
    .await;

    // Assert
    let session_names: Vec<&str> = app
        .sessions
        .sessions()
        .iter()
        .map(|session| session.id.as_str())
        .collect();
    assert_eq!(session_names, vec!["beta0000", "alpha000"]);
}

#[test]
/// Ensures reasoning reducer updates only the matching in-memory session
/// snapshot and leaves unrelated sessions untouched.
fn test_apply_session_reasoning_level_updated_updates_only_matching_session() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", Some(ReasoningLevel::Low));

    // Act
    session_manager.apply_session_reasoning_level_updated("other-session", ReasoningLevel::High);
    let reasoning_level_after_non_matching_update =
        session_manager.state.sessions[0].reasoning_level_override;
    session_manager.apply_session_reasoning_level_updated("session-id", ReasoningLevel::Medium);

    // Assert
    assert_eq!(
        reasoning_level_after_non_matching_update,
        Some(ReasoningLevel::Low)
    );
    assert_eq!(
        session_manager.state.sessions[0].reasoning_level_override,
        Some(ReasoningLevel::Medium)
    );
}

#[test]
/// Ensures response-style reducer updates only the matching in-memory session
/// snapshot and leaves unrelated sessions untouched.
fn test_apply_session_response_style_updated_updates_only_matching_session() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);

    // Act
    session_manager.apply_session_response_style_updated("other-session", ResponseStyle::Detailed);
    let style_after_non_matching_update = session_manager.state.sessions[0].response_style;
    session_manager.apply_session_response_style_updated("session-id", ResponseStyle::Concise);

    // Assert
    assert_eq!(style_after_non_matching_update, ResponseStyle::Balanced);
    assert_eq!(
        session_manager.state.sessions[0].response_style,
        ResponseStyle::Concise
    );
}

#[test]
/// Ensures speed reducer updates only the matching in-memory session
/// snapshot and leaves unrelated sessions untouched.
fn test_apply_session_speed_mode_updated_updates_only_matching_session() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);

    // Act
    session_manager.apply_session_speed_mode_updated("other-session", SpeedMode::Fast);
    let speed_mode_after_non_matching_update = session_manager.state.sessions[0].speed_mode;
    session_manager.apply_session_speed_mode_updated("session-id", SpeedMode::Fast);

    // Assert
    assert_eq!(speed_mode_after_non_matching_update, SpeedMode::Normal);
    assert_eq!(
        session_manager.state.sessions[0].speed_mode,
        SpeedMode::Fast
    );
}

#[test]
/// Ensures permission reducer updates only the matching in-memory session
/// snapshot and leaves unrelated sessions untouched.
fn test_apply_session_permission_mode_updated_updates_only_matching_session() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);

    // Act
    session_manager
        .apply_session_permission_mode_updated("other-session", PermissionMode::ReadOnly);
    let mode_after_non_matching_update = session_manager.state.sessions[0].permission_mode;
    session_manager.apply_session_permission_mode_updated("session-id", PermissionMode::ReadOnly);

    // Assert
    assert_eq!(mode_after_non_matching_update, PermissionMode::AutoEdit);
    assert_eq!(
        session_manager.state.sessions[0].permission_mode,
        PermissionMode::ReadOnly
    );
}

#[test]
fn test_update_orchestration_progress_replaces_and_clears_board_snapshot() {
    // Arrange
    let mut session_manager = test_session_manager("controller", None);

    // Act
    session_manager.update_orchestration_progress(
        "controller",
        Some("Working... Protocol: running".to_string()),
    );
    session_manager.update_orchestration_progress(
        "controller",
        Some("Working... Protocol: ready".to_string()),
    );

    // Assert
    assert_eq!(
        session_manager.sessions()[0]
            .orchestration_progress
            .as_deref(),
        Some("Working... Protocol: ready")
    );
    assert!(
        session_manager.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::Orchestration)
            .is_none()
    );

    // Act
    session_manager.update_orchestration_progress("controller", None);
    session_manager.update_orchestration_progress("missing", Some("ignored".to_string()));

    // Assert
    assert!(
        session_manager.sessions()[0]
            .orchestration_progress
            .is_none()
    );
}

#[tokio::test]
async fn test_reply_turn_completion_persists_session_size() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().returning(|request| {
        let mut command = Command::new("sh");
        command
            .args([
                "-lc",
                "yes line | head -n 20 > turn-size-test.txt; echo turn-complete",
            ])
            .current_dir(request.folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        Ok(command)
    });

    // Act
    app.sessions
        .reply_with_backend(
            &app.services,
            &session_id,
            "compute size after turn",
            Arc::new(backend),
            AgentModel::ClaudeOpus5,
        )
        .await;
    wait_for_status_with_retries(&mut app, &session_id, Status::AgentReview, 200, true).await;
    app.process_pending_app_events().await;
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing in-memory session");
    let db_session = db_sessions
        .iter()
        .find(|db_session| db_session.id == session_id)
        .expect("missing persisted session");
    assert_eq!(session.size, SessionSize::S);
    assert_eq!(session.stats.added_lines, 20);
    assert_eq!(session.stats.deleted_lines, 0);
    assert_eq!(db_session.added_lines, 20);
    assert_eq!(db_session.deleted_lines, 0);
    assert_eq!(db_session.size, "S");
}
