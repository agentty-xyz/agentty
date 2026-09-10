use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use ag_forge as forge;
use app::branch_publish::{
    BranchPublishTaskFailure, BranchPublishTaskSession, push_session_branch,
};
use mockall::predicate::eq;
use tempfile::tempdir;

use super::super::App;
use super::support::{new_test_app_with_selected_session, seed_selected_session_empty_diff_state};
use crate::app;
use crate::app::branch_publish::BranchPublishTaskSuccess;
use crate::app::core::event::AppEvent;
use crate::app::{AppError, session};
use crate::domain::session::{PublishBranchAction, SESSION_DATA_DIR, SessionDiffState, Status};
use crate::infra::db::AppRepositories;
use crate::infra::tmux::MockTmuxClient;
use crate::presentation::app_mode::{
    AppMode, DiffCommentTarget, DiffFocus, DiffLineComments, DiffPreview, DiffReviewComments,
};

/// Verifies pushing a review session surfaces forge-specific git
/// authentication guidance when the remote rejects credentials.
#[tokio::test]
async fn push_session_branch_auth_failure_shows_git_guidance() {
    // Arrange
    let branch_session = BranchPublishTaskSession::from_session(
        &crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/review-session")),
    );
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some(session::session_branch("session-1")) }));
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .once()
        .with(
            mockall::predicate::eq(PathBuf::from("/tmp/review-session")),
            mockall::predicate::eq(session::session_branch("session-1")),
        )
        .returning(|_, _| {
            Box::pin(async {
                Err(ag_git::GitError::OutputParse(
                    "Git push failed: fatal: could not read Username for 'https://github.com': \
                     terminal prompts disabled"
                        .to_string(),
                ))
            })
        });
    let git_client: Arc<dyn ag_git::GitClient> = Arc::new(mock_git_client);
    let database = crate::infra::db::AppRepositories::in_memory()
        .await
        .expect("db should open");

    // Act
    let result = push_session_branch(
        PublishBranchAction::Push,
        &branch_session,
        database,
        git_client,
        None,
    )
    .await;

    // Assert
    assert!(matches!(
        result,
        Err(BranchPublishTaskFailure {
            ref title,
            ref message,
            ..
        }) if title == "Branch push blocked"
            && message.contains("Git push requires authentication")
            && message.contains("gh auth login")
    ));
}

#[tokio::test]
async fn push_session_branch_preserves_blocked_when_remote_branch_exists() {
    // Arrange
    let branch_session = BranchPublishTaskSession::from_session(
        &crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/review-session")),
    );
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some(session::session_branch("session-1")) }));
    mock_git_client
        .expect_remote_branch_exists()
        .once()
        .returning(|_, _| Box::pin(async { Ok(true) }));
    let git_client: Arc<dyn ag_git::GitClient> = Arc::new(mock_git_client);
    let database = crate::infra::db::AppRepositories::in_memory()
        .await
        .expect("db should open");

    // Act
    let result = push_session_branch(
        PublishBranchAction::Push,
        &branch_session,
        database,
        git_client,
        Some("feature/existing"),
    )
    .await;

    // Assert
    let failure = result.expect_err("push should be blocked");
    assert_eq!(failure.title, "Branch push blocked");
    assert!(failure.message.contains("already exists"));
}

#[tokio::test]
async fn push_session_branch_shows_auth_guidance_when_ls_remote_fails_with_auth_error() {
    // Arrange
    let branch_session = BranchPublishTaskSession::from_session(
        &crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/review-session")),
    );
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some(session::session_branch("session-1")) }));
    mock_git_client
        .expect_remote_branch_exists()
        .once()
        .returning(|_, _| {
            Box::pin(async {
                Err(ag_git::GitError::CommandFailed {
                    command: "git ls-remote".to_string(),
                    stderr: "fatal: could not read Username for 'https://github.com/org/repo': \
                             terminal prompts disabled"
                        .to_string(),
                })
            })
        });
    let git_client: Arc<dyn ag_git::GitClient> = Arc::new(mock_git_client);
    let database = crate::infra::db::AppRepositories::in_memory()
        .await
        .expect("db should open");

    // Act
    let result = push_session_branch(
        PublishBranchAction::Push,
        &branch_session,
        database,
        git_client,
        Some("feature/new"),
    )
    .await;

    // Assert
    let failure = result.expect_err("push should be blocked");
    assert_eq!(failure.title, "Branch push blocked");
    assert!(failure.message.contains("Git push requires authentication"));
    assert!(failure.message.contains("gh auth login"));
}

#[tokio::test]
async fn push_session_branch_uses_custom_remote_branch_name_when_provided() {
    // Arrange
    let branch_session = BranchPublishTaskSession::from_session(
        &crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/review-session")),
    );
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_remote_branch_exists()
        .once()
        .returning(|_, _| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some(session::session_branch("session-1")) }));
    mock_git_client
        .expect_push_current_branch_to_new_remote_branch()
        .with(
            mockall::predicate::eq(PathBuf::from("/tmp/review-session")),
            mockall::predicate::eq("review/custom-branch".to_string()),
        )
        .once()
        .returning(|_, _| Box::pin(async { Ok("origin/review/custom-branch".to_string()) }));
    mock_git_client
        .expect_repo_url()
        .with(mockall::predicate::eq(PathBuf::from("/tmp/review-session")))
        .once()
        .returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
    let git_client: Arc<dyn ag_git::GitClient> = Arc::new(mock_git_client);
    let database = crate::infra::db::AppRepositories::in_memory()
        .await
        .expect("db should open");

    // Act
    let result = push_session_branch(
        PublishBranchAction::Push,
        &branch_session,
        database.clone(),
        git_client,
        Some("review/custom-branch"),
    )
    .await;

    // Assert
    assert_eq!(
            result,
            Ok(BranchPublishTaskSuccess::Pushed {
                branch_name: "review/custom-branch".to_string(),
                review_request_creation: Some(crate::app::branch_publish::ReviewRequestCreationInfo {
                    forge_kind: forge::ForgeKind::GitHub,
                    web_url: Some(
                        "https://github.com/agentty-xyz/agentty/compare/main...review%2Fcustom-branch?expand=1"
                            .to_string()
                    ),
                }),
                upstream_reference: "origin/review/custom-branch".to_string(),
            })
        );
}

#[tokio::test]
async fn apply_turn_started_clears_saved_diff_comments() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut line_comments = DiffLineComments::default();
    line_comments.start_editing_target(DiffCommentTarget::file("src/main.rs"));
    app.save_diff_comment_progress("session-1".into(), line_comments);

    // Act
    app.apply_app_events(AppEvent::SessionTurnStarted {
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    assert!(!app.diff_comment_progress.contains_key("session-1"));
}

#[tokio::test]
async fn configured_launch_configurations_returns_trimmed_non_empty_entries() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.settings.launch_configuration = "  cargo test \n npm run dev \n".to_string();

    // Act
    let launch_configurations = app.configured_launch_configurations();

    // Assert
    assert_eq!(
        launch_configurations,
        vec!["cargo test".to_string(), "npm run dev".to_string()]
    );
}

#[tokio::test]
async fn test_continue_terminal_session_rejects_non_terminal_source_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let source_session = crate::test_support::SessionFixtureBuilder::new()
        .id("review-source")
        .status(Status::Review)
        .build();
    app.sessions.push_session(source_session);

    // Act
    let result = app.continue_terminal_session("review-source").await;

    // Assert
    assert!(matches!(
        result,
        Err(AppError::Workflow(message))
            if message == "Only `Done` or `Canceled` sessions can be continued"
    ));
}

#[tokio::test]
async fn open_session_worktree_in_tmux_runs_configured_launch_configuration_when_window_opens() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session-launch-configuration");
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client
        .expect_open_window_for_folder()
        .with(eq(session_folder))
        .times(1)
        .returning(|_| Box::pin(async { Some("@42".to_string()) }));
    mock_tmux_client
        .expect_run_command_in_window()
        .with(eq("@42".to_string()), eq("npm run dev".to_string()))
        .times(1)
        .returning(|_, _| Box::pin(async {}));
    let mut app = new_test_app_with_selected_session(
        PathBuf::from("/tmp/session-launch-configuration"),
        "  npm run dev  ",
        Arc::new(mock_tmux_client),
    )
    .await;
    seed_selected_session_empty_diff_state(&mut app).await;

    // Act
    app.open_session_worktree_in_tmux().await;

    // Assert
    let persisted_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load persisted session");
    let persisted_session = persisted_sessions
        .iter()
        .find(|session| session.id == "session-1")
        .expect("missing persisted session");
    assert_eq!(
        app.sessions.sessions()[0].stats.diff_state,
        SessionDiffState::Unknown
    );
    assert_eq!(persisted_session.has_diff, None);
}

#[tokio::test]
async fn open_session_worktree_in_tmux_keeps_invalidation_when_window_open_fails() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session-window-open-failure");
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client
        .expect_open_window_for_folder()
        .with(eq(session_folder.clone()))
        .times(1)
        .returning(|_| Box::pin(async { None }));
    mock_tmux_client.expect_run_command_in_window().times(0);
    let mut app = new_test_app_with_selected_session(
        session_folder,
        "npm run dev",
        Arc::new(mock_tmux_client),
    )
    .await;
    seed_selected_session_empty_diff_state(&mut app).await;

    // Act
    app.open_session_worktree_in_tmux().await;

    // Assert
    let persisted_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load persisted session");
    let persisted_session = persisted_sessions
        .iter()
        .find(|session| session.id == "session-1")
        .expect("missing persisted session");
    assert_eq!(
        app.sessions.sessions()[0].stats.diff_state,
        SessionDiffState::Unknown
    );
    assert_eq!(persisted_session.has_diff, None);
}

#[tokio::test]
async fn open_session_worktree_in_tmux_is_disabled_outside_tmux() {
    // Arrange
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let session_folder = temp_dir.path().join("session-worktree");
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client.expect_open_window_for_folder().times(0);
    mock_tmux_client.expect_run_command_in_window().times(0);
    let mut app = new_test_app_with_selected_session(
        session_folder,
        "cargo test",
        Arc::new(mock_tmux_client),
    )
    .await;
    app.is_tmux_session = false;
    app.sessions.sessions_mut()[0].stats.diff_state = SessionDiffState::Empty;

    // Act
    app.open_session_worktree_in_tmux().await;

    // Assert
    assert_eq!(
        app.sessions.sessions()[0].stats.diff_state,
        SessionDiffState::Empty
    );
}

#[tokio::test]
async fn open_session_worktree_in_tmux_skips_missing_worktree_folder() {
    // Arrange
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let missing_session_folder = temp_dir.path().join("missing-session-worktree");
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client.expect_open_window_for_folder().times(0);
    mock_tmux_client.expect_run_command_in_window().times(0);
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(mock_tmux_client),
    )
    .await;
    app.settings.launch_configuration = "npm run dev".to_string();
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            missing_session_folder,
        ));
    app.sessions.select_session_index(Some(0));

    // Act
    app.open_session_worktree_in_tmux().await;

    // Assert
    // Expectations are validated by `mockall`.
}

#[tokio::test]
async fn open_session_worktree_in_tmux_uses_first_configured_command() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session-multiple-launch-configurations");
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client
        .expect_open_window_for_folder()
        .with(eq(session_folder))
        .times(1)
        .returning(|_| Box::pin(async { Some("@42".to_string()) }));
    mock_tmux_client
        .expect_run_command_in_window()
        .with(eq("@42".to_string()), eq("cargo test".to_string()))
        .times(1)
        .returning(|_, _| Box::pin(async {}));
    let mut app = new_test_app_with_selected_session(
        PathBuf::from("/tmp/session-multiple-launch-configurations"),
        " cargo test \n npm run dev ",
        Arc::new(mock_tmux_client),
    )
    .await;

    // Act
    app.open_session_worktree_in_tmux().await;

    // Assert
    // Expectations are validated by `mockall`.
}

#[tokio::test]
async fn diff_comments_have_no_tick_driven_ui() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Diff {
        diff: String::new(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: Some(DiffReviewComments::loading(1)),
        restore: None,
        scroll_cache: None,
        session_id: "session-id".into(),
        scroll_offset: 0,
    };

    // Act
    let has_tick_driven_ui = app.has_visible_tick_driven_ui();

    // Assert
    assert!(!has_tick_driven_ui);
}

#[tokio::test]
/// Ensures startup selection prefers active sessions over archive rows.
async fn test_new_prefers_active_session_for_initial_selection() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to upsert project");
    let active_session_id = "z-active-session";
    let archive_session_id = "a-archive-session";
    database
        .sessions()
        .insert_session(
            active_session_id,
            "gemini-3.8-flash",
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert active session");
    database
        .sessions()
        .insert_session(
            archive_session_id,
            "gemini-3.8-flash",
            "main",
            &Status::Done.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert archived session");

    let active_folder_name = active_session_id.chars().take(8).collect::<String>();
    let active_session_data_dir = base_path.join(active_folder_name).join(SESSION_DATA_DIR);
    fs::create_dir_all(active_session_data_dir).expect("failed to create active session dir");

    // Act
    let app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");

    // Assert
    assert_eq!(
        app.selected_session().map(|session| session.id.as_str()),
        Some(active_session_id)
    );
}

#[tokio::test]
async fn test_new_with_clients_fails_when_no_backend_cli_is_available() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let clients = crate::test_support::test_app_clients_with_available_agent_kinds(Vec::new())
        .with_app_server_client_override(crate::test_support::mock_app_server())
        .with_tmux_client(Arc::new(MockTmuxClient::new()));

    // Act
    let result = App::new_with_clients(base_path.clone(), base_path, None, database, clients).await;

    // Assert
    assert!(matches!(
        result,
        Err(AppError::Workflow(message))
            if message
                == "No supported backend CLI found on `PATH`. Install `codex`, `claude`, `gemini`, or Antigravity CLI 1.1.7 or newer. For an older `agy`, run `agy update`, then restart `agentty`."
    ));
}
