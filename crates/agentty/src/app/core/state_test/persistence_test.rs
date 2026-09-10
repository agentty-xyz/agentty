use std::collections::HashMap;
use std::fs;
use std::sync::Arc;

use tempfile::tempdir;

use super::super::App;
use super::support::install_mock_git_client;
use crate::app::Tab;
use crate::domain::session::{SessionDiffState, Status};
use crate::domain::setting::SettingName;
use crate::infra::db::AppRepositories;
use crate::infra::tmux::MockTmuxClient;
use crate::presentation::app_mode::AppMode;

#[tokio::test]
async fn test_continue_terminal_session_falls_back_to_persisted_context_without_hash() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let clients = crate::test_support::test_app_clients()
        .with_app_server_client_override(crate::test_support::mock_app_server())
        .with_tmux_client(Arc::new(MockTmuxClient::new()));
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        Some("main".to_string()),
        database,
        clients,
    )
    .await
    .expect("failed to build test app");
    let project_id = app.active_project_id();
    app.services
        .db()
        .sessions()
        .insert_session("done-source", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert source session row");
    let source_session = crate::test_support::SessionFixtureBuilder::new()
        .id("done-source")
        .status(Status::Done)
        .title(Some("Done source".to_string()))
        .transcript("Use the saved context.")
        .build();
    app.sessions.push_session(source_session);
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .never()
        .returning(|path| Box::pin(async move { Some(path) }));
    mock_git_client
        .expect_fetch_remote()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    mock_git_client
        .expect_get_ref_ahead_behind()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok((0, 0)) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    let continued_session_id = app
        .continue_terminal_session("done-source")
        .await
        .expect("expected done continuation to succeed");

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Prompt {
            ref input,
            ref session_id,
            ..
        } if session_id.as_str() == continued_session_id && input.text().is_empty()
    ));
    let continued_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == continued_session_id)
        .expect("expected created continuation draft");
    assert_eq!(
        continued_session.prompt,
        "Continue the work from this previous Agentty session.\n\nPrevious session: Done \
         source\nProject: project\nStatus: Done\n\nPrevious session transcript:\nUse the saved \
         context.\n"
    );
}

#[tokio::test]
async fn test_continue_terminal_session_uses_persisted_context_for_canceled_source_session() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let clients = crate::test_support::test_app_clients()
        .with_app_server_client_override(crate::test_support::mock_app_server())
        .with_tmux_client(Arc::new(MockTmuxClient::new()));
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        Some("main".to_string()),
        database,
        clients,
    )
    .await
    .expect("failed to build test app");
    let project_id = app.active_project_id();
    app.services
        .db()
        .sessions()
        .insert_session(
            "canceled-source",
            "gpt-5.6-sol",
            "main",
            "Canceled",
            project_id,
        )
        .await
        .expect("failed to insert source session row");
    app.services
        .db()
        .sessions()
        .update_session_merged_commit_hash("canceled-source", Some("stale-merged-hash".to_string()))
        .await
        .expect("failed to persist stale merged commit hash");
    let source_session = crate::test_support::SessionFixtureBuilder::new()
        .id("canceled-source")
        .status(Status::Canceled)
        .title(Some("Canceled source".to_string()))
        .transcript("Resume the remaining work.")
        .build();
    app.sessions.push_session(source_session);
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .never()
        .returning(|path| Box::pin(async move { Some(path) }));
    mock_git_client
        .expect_fetch_remote()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    mock_git_client
        .expect_get_ref_ahead_behind()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok((0, 0)) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    let continued_session_id = app
        .continue_terminal_session("canceled-source")
        .await
        .expect("expected canceled continuation to succeed");

    // Assert
    let continued_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == continued_session_id)
        .expect("expected created continuation draft");
    assert_eq!(continued_session.status, Status::Draft);
    assert_eq!(
        continued_session.prompt,
        "Continue the work from this previous Agentty session.\n\nPrevious session: Canceled \
         source\nProject: project\nStatus: Canceled\n\nPrevious session transcript:\nResume the \
         remaining work.\n"
    );
    assert!(!continued_session.prompt.contains("stale-merged-hash"));
    assert!(matches!(
        app.mode,
        AppMode::Prompt {
            ref input,
            ref session_id,
            ..
        } if session_id.as_str() == continued_session_id && input.text().is_empty()
    ));
}

#[tokio::test]
async fn open_session_worktree_in_tmux_stays_closed_when_persistence_fails() {
    // Arrange
    let (mut app, base_dir, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let session_folder = base_dir.path().join("session-persistence-failure");
    fs::create_dir_all(&session_folder).expect("failed to create session folder");
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client.expect_open_window_for_folder().times(0);
    mock_tmux_client.expect_run_command_in_window().times(0);
    app.tmux_client = Arc::new(mock_tmux_client);
    app.is_tmux_session = true;
    let mut session = crate::test_support::session_fixture_with_folder(session_folder);
    session.stats.diff_state = SessionDiffState::Empty;
    app.sessions.push_session(session);
    app.sessions.select_session_index(Some(0));
    pool.close().await;

    // Act
    app.open_session_worktree_in_tmux_with_command(None).await;

    // Assert
    assert_eq!(
        app.sessions.sessions()[0].stats.diff_state,
        SessionDiffState::Empty
    );
}

#[tokio::test]
async fn test_persist_current_tab_stores_active_tab() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.tabs.set(Tab::Settings);

    // Act
    app.persist_current_tab().await;

    // Assert
    let persisted_tab = app
        .services
        .db()
        .settings()
        .get_setting(SettingName::ActiveTab)
        .await
        .expect("failed to load active tab");
    assert_eq!(persisted_tab.as_deref(), Some(Tab::Settings.as_str()));
}

#[tokio::test]
async fn test_new_with_clients_restores_persisted_active_tab() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let agentty_home = temp_dir.path().join("agentty-home");
    let project_path = temp_dir.path().join("project");
    fs::create_dir_all(&agentty_home).expect("failed to create agentty home");
    fs::create_dir_all(project_path.join(".git")).expect("failed to create project git marker");
    let database = AppRepositories::in_memory().await.expect("db should open");
    database
        .settings()
        .upsert_setting(SettingName::ActiveTab, Tab::Settings.as_str())
        .await
        .expect("failed to persist active tab");

    // Act
    let app = App::new_with_clients(
        agentty_home,
        project_path,
        Some("main".to_string()),
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");

    // Assert
    assert_eq!(app.tabs.current(), Tab::Settings);
}
