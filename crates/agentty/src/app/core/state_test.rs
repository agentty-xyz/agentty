use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ag_protocol::AgentResponseSummary;
use mockall::predicate::eq;
use tempfile::tempdir;

use super::*;
use crate::app::branch_publish::{BranchPublishActionUpdate, BranchPublishTaskSuccess};
use crate::app::review::ReviewUpdate;
use crate::app::session_state::SessionGitStatus;
use crate::app::{AppServiceDeps, diff_content_hash};
use crate::domain::agent::AgentModel;
use crate::domain::file_entry::FileEntry;
use crate::domain::question::QuestionItem;
use crate::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SESSION_DATA_DIR,
    SessionFollowUpTask, SessionHandles, SessionSize, SessionStats, Status,
};
use crate::domain::session_message::{SessionMessageKind, SessionMessageState, SessionTranscript};
use crate::domain::setting::SettingName;
use crate::infra::db::AppRepositories;
use crate::infra::project_discovery::{HOME_PROJECT_SCAN_MAX_RESULTS, RealProjectDiscoveryClient};
use crate::infra::tmux::{MockTmuxClient, TmuxClient};
use crate::presentation::app_mode::ConfirmationViewMode;

/// Builds one reducer-ready turn projection for tests.
fn test_turn_applied_state(
    questions: Vec<QuestionItem>,
    follow_up_tasks: Vec<&str>,
    _summary: Option<AgentResponseSummary>,
    token_usage_delta: SessionStats,
) -> TurnAppliedState {
    TurnAppliedState {
        follow_up_tasks: follow_up_tasks
            .into_iter()
            .enumerate()
            .map(|(position, text)| SessionFollowUpTask {
                id: i64::try_from(position).unwrap_or(i64::MAX),
                launched_session_id: None,
                position,
                text: text.to_string(),
            })
            .collect(),
        questions,
        token_usage_delta,
    }
}

/// Builds a restore-view snapshot used by branch-publish event-batch
/// tests.
fn test_confirmation_view_mode(session_id: &str) -> ConfirmationViewMode {
    ConfirmationViewMode {
        scroll_offset: None,
        session_id: session_id.into(),
    }
}

fn test_view_app_mode(session_id: &str) -> AppMode {
    AppMode::View {
        session_id: session_id.into(),
        scroll_offset: None,
    }
}

async fn test_app_viewing_reconcile_session(
    status: Status,
    questions: Vec<QuestionItem>,
    folder_name: &str,
) -> App {
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("session-1")
            .folder(PathBuf::from(format!("/tmp/{folder_name}")))
            .status(status)
            .questions(questions)
            .build(),
    );
    app.mode = test_view_app_mode("session-1");

    app
}

/// Persists one session row for timeline tests with manually assembled state.
async fn persist_timeline_test_session(app: &App, session_id: &str, status: Status) {
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            AgentModel::Gemini3FlashPreview.as_str(),
            "main",
            &status.to_string(),
            app.active_project_id(),
        )
        .await
        .expect("failed to persist timeline test session");
}

/// Builds one successful branch-publish batch payload for one session.
fn test_pushed_branch_result(branch_name: &str) -> BranchPublishTaskSuccess {
    BranchPublishTaskSuccess::Pushed {
        branch_name: branch_name.to_string(),
        review_request_creation: None,
        upstream_reference: format!("origin/{branch_name}"),
    }
}

/// Verifies app startup seeds the first process-local system log entry.
#[tokio::test]
async fn new_with_clients_records_startup_system_log() {
    // Arrange, Act
    let app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;

    // Assert
    assert!(
        app.system_logs
            .entries()
            .iter()
            .any(|entry| entry.message == "Agentty started")
    );
}

#[tokio::test]
async fn test_new_with_clients_fails_when_no_backend_cli_is_available() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await;
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
                == "No supported backend CLI found on `PATH`. Install `agy`, `codex`, `claude`, or `gemini` and restart `agentty`."
    ));
}

#[tokio::test]
async fn session_git_status_targets_include_active_unpublished_sessions() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let review_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-review"));
    let mut done_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-done"));
    done_session.id = "session-2".into();
    done_session.status = Status::Done;
    app.sessions.push_session(review_session);
    app.sessions.push_session(done_session);

    // Act
    let targets = App::session_git_status_targets(&app.sessions);

    // Assert
    assert_eq!(
        targets,
        vec![sync::SessionGitStatusTarget {
            base_branch: "main".to_string(),
            branch_name: "wt/session-".to_string(),
            session_id: "session-1".into(),
        }]
    );
}

#[tokio::test]
async fn session_git_status_targets_use_detected_session_branch_name_when_available() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let review_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-review"));
    app.sessions.push_session(review_session);
    app.sessions.replace_session_branch_names(HashMap::from([(
        SessionId::from("session-1"),
        "agentty/session-".to_string(),
    )]));

    // Act
    let targets = App::session_git_status_targets(&app.sessions);

    // Assert
    assert_eq!(
        targets,
        vec![sync::SessionGitStatusTarget {
            base_branch: "main".to_string(),
            branch_name: "agentty/session-".to_string(),
            session_id: "session-1".into(),
        }]
    );
}

#[tokio::test]
async fn session_git_status_targets_skip_unmaterialized_drafts() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut draft_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-draft"));
    draft_session.id = "draft-1".into();
    draft_session.is_draft = true;
    draft_session.status = Status::Draft;
    app.sessions.push_session(draft_session);

    // Act
    let targets_before_materialization = App::session_git_status_targets(&app.sessions);
    app.sessions.set_session_worktree_available("draft-1", true);
    let targets_after_materialization = App::session_git_status_targets(&app.sessions);

    // Assert
    assert!(targets_before_materialization.is_empty());
    assert_eq!(
        targets_after_materialization,
        vec![sync::SessionGitStatusTarget {
            base_branch: "main".to_string(),
            branch_name: "wt/draft-1".to_string(),
            session_id: "draft-1".into(),
        }]
    );
}

#[tokio::test]
async fn test_switch_project_reloads_project_scoped_settings() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let second_project_dir = tempdir().expect("failed to create second temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await;
    let first_project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project(&second_project_dir.path().to_string_lossy(), None)
        .await
        .expect("failed to insert second project");
    database
        .settings()
        .upsert_project_setting(
            first_project_id,
            SettingName::DefaultSmartModel,
            AgentModel::ClaudeHaiku4520251001.as_str(),
        )
        .await
        .expect("failed to persist first project smart model");
    database
        .settings()
        .upsert_project_setting(
            first_project_id,
            SettingName::LaunchConfiguration,
            "npm run dev",
        )
        .await
        .expect("failed to persist first project launch configuration");
    database
        .settings()
        .upsert_project_setting(
            second_project_id,
            SettingName::DefaultSmartModel,
            AgentModel::Gpt55.as_str(),
        )
        .await
        .expect("failed to persist second project smart model");
    database
        .settings()
        .upsert_project_setting(
            second_project_id,
            SettingName::LaunchConfiguration,
            "cargo test",
        )
        .await
        .expect("failed to persist second project launch configuration");
    database
        .settings()
        .set_active_project_id(first_project_id)
        .await
        .expect("failed to persist initial active project");
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");

    // Act
    app.switch_project(second_project_id)
        .await
        .expect("failed to switch project");

    // Assert
    assert_eq!(
        app.settings.default_smart_selection.model(),
        AgentModel::Gpt55
    );
    assert_eq!(app.settings.launch_configuration, "cargo test");
}

#[tokio::test]
async fn test_switch_project_updates_active_git_upstream_reference() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let second_project_dir = tempdir().expect("failed to create second temp dir");
    let base_path = base_dir.path().to_path_buf();
    let second_project_path = second_project_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await;
    let first_project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project(&second_project_path.to_string_lossy(), None)
        .await
        .expect("failed to insert second project");
    database
        .settings()
        .set_active_project_id(first_project_id)
        .await
        .expect("failed to persist initial active project");
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("feature/footer-bar".to_string()) }));
    mock_git_client
        .expect_current_upstream_reference()
        .once()
        .returning(|_| Box::pin(async { Ok("origin/feature/footer-bar".to_string()) }));
    mock_git_client
        .expect_find_git_repo_root()
        .times(0..)
        .returning(|path| Box::pin(async move { Some(path) }));
    mock_git_client
        .expect_fetch_remote()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.switch_project(second_project_id)
        .await
        .expect("failed to switch project");

    // Assert
    assert_eq!(app.git_branch(), Some("feature/footer-bar"));
    assert_eq!(app.git_upstream_ref(), Some("origin/feature/footer-bar"));
}

#[tokio::test]
async fn open_selected_requested_review_surfaces_comment_load_error() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.replace_requested_reviews(app.projects.active_project_id(), vec![requested_review()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    install_mock_git_client(&mut app, mock_git_client);

    let mut mock_review_request_client = forge::MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(forge_remote()));
    mock_review_request_client
        .expect_fetch_review_comment_snapshot()
        .once()
        .returning(|_, _| {
            Box::pin(async {
                Err(forge::ReviewRequestError::OperationFailed {
                    forge_kind: forge::ForgeKind::GitHub,
                    message: "authentication failed".to_string(),
                })
            })
        });
    install_mock_review_request_client(&mut app, mock_review_request_client);

    // Act
    app.open_selected_requested_review();

    // Assert
    assert_eq!(app.requested_review_comment_fetches.len(), 1);
    assert!(matches!(
        app.mode,
        AppMode::ReviewDetail {
            comment_error: None,
            is_loading_comments: true,
            ref review,
            scroll_offset: 0,
        } if review.comment_snapshot.is_none()
    ));

    // Act
    wait_for_app_condition(&mut app, |app| {
        matches!(
            app.mode,
            AppMode::ReviewDetail {
                comment_error: Some(_),
                is_loading_comments: false,
                ..
            }
        )
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::ReviewDetail {
            comment_error: Some(ref comment_error),
            is_loading_comments: false,
            ref review,
            scroll_offset: 0,
        } if comment_error.contains("Failed to load review comments:")
            && comment_error.contains("authentication failed")
            && review.comment_snapshot.is_none()
    ));
    assert!(app.requested_review_comment_fetches.is_empty());
}

#[tokio::test]
async fn open_selected_requested_review_applies_background_comment_snapshot() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.replace_requested_reviews(app.projects.active_project_id(), vec![requested_review()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    install_mock_git_client(&mut app, mock_git_client);

    let mut mock_review_request_client = forge::MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(forge_remote()));
    mock_review_request_client
        .expect_fetch_review_comment_snapshot()
        .once()
        .returning(|_, _| Box::pin(async { Ok(review_comment_snapshot()) }));
    install_mock_review_request_client(&mut app, mock_review_request_client);

    // Act
    app.open_selected_requested_review();

    // Assert
    assert_eq!(app.requested_review_comment_fetches.len(), 1);
    assert!(matches!(
        app.mode,
        AppMode::ReviewDetail {
            comment_error: None,
            is_loading_comments: true,
            ref review,
            scroll_offset: 0,
        } if review.comment_snapshot.is_none()
    ));

    // Act
    wait_for_app_condition(&mut app, |app| {
        matches!(
            app.mode,
            AppMode::ReviewDetail {
                comment_error: None,
                is_loading_comments: false,
                ref review,
                ..
            } if review.comment_snapshot.is_some()
        )
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::ReviewDetail {
            comment_error: None,
            is_loading_comments: false,
            ref review,
            scroll_offset: 0,
        } if review.comment_snapshot == Some(review_comment_snapshot())
    ));
    let expected_comment_snapshot = review_comment_snapshot();
    assert_eq!(
        app.selected_requested_review()
            .and_then(|review| review.comment_snapshot.as_ref()),
        Some(&expected_comment_snapshot)
    );
    assert!(app.requested_review_comment_fetches.is_empty());
}

/// Verifies reopening a loading requested-review detail reuses the
/// existing background comment fetch.
#[tokio::test]
async fn open_selected_requested_review_reuses_in_flight_comment_fetch() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.replace_requested_reviews(app.projects.active_project_id(), vec![requested_review()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    install_mock_git_client(&mut app, mock_git_client);

    let mut mock_review_request_client = forge::MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(forge_remote()));
    mock_review_request_client
        .expect_fetch_review_comment_snapshot()
        .once()
        .returning(|_, _| Box::pin(async { Ok(review_comment_snapshot()) }));
    install_mock_review_request_client(&mut app, mock_review_request_client);

    // Act
    app.open_selected_requested_review();
    app.mode = AppMode::List;
    app.open_selected_requested_review();

    // Assert
    assert_eq!(app.requested_review_comment_fetches.len(), 1);
    assert!(matches!(
        app.mode,
        AppMode::ReviewDetail {
            comment_error: None,
            is_loading_comments: true,
            ref review,
            scroll_offset: 0,
        } if review.comment_snapshot.is_none()
    ));

    // Act
    wait_for_app_condition(&mut app, |app| {
        matches!(
            app.mode,
            AppMode::ReviewDetail {
                comment_error: None,
                is_loading_comments: false,
                ref review,
                ..
            } if review.comment_snapshot.is_some()
        )
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::ReviewDetail {
            comment_error: None,
            is_loading_comments: false,
            ref review,
            scroll_offset: 0,
        } if review.comment_snapshot == Some(review_comment_snapshot())
    ));
    assert!(app.requested_review_comment_fetches.is_empty());
}

/// Verifies an explicit Inbox tab refresh prevents older in-flight
/// comment snapshots from repopulating the refreshed list row.
#[tokio::test]
async fn refresh_requested_reviews_ignores_stale_comment_snapshot_completion() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.tabs.set(Tab::Review);
    let project_id = app.projects.active_project_id();
    let review = requested_review();
    let display_id = review.display_id.clone();
    let web_url = review.web_url.clone();
    app.replace_requested_reviews(project_id, vec![review]);
    let stale_generation = app.requested_review_generation;
    assert!(
        app.requested_review_comment_fetches
            .insert(RequestedReviewCommentFetchKey {
                display_id: display_id.clone(),
                generation: stale_generation,
                project_id,
                web_url: web_url.clone(),
            })
    );

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    install_mock_git_client(&mut app, mock_git_client);

    let mut mock_review_request_client = forge::MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(forge_remote()));
    mock_review_request_client
        .expect_list_requested_reviews()
        .once()
        .returning(|_| Box::pin(async { Ok(vec![requested_review()]) }));
    install_mock_review_request_client(&mut app, mock_review_request_client);

    // Act
    app.refresh_requested_reviews_for_current_project();

    // Assert
    assert!(app.requested_review_comment_fetches.is_empty());

    // Act
    wait_for_app_condition(&mut app, |app| {
        matches!(
            app.requested_reviews,
            RequestedReviewState::Loaded {
                ref items,
                project_id: loaded_project_id,
            } if loaded_project_id == project_id
                && items.len() == 1
                && items[0].comment_snapshot.is_none()
        )
    })
    .await;
    let current_generation_fetch_key = RequestedReviewCommentFetchKey {
        display_id: display_id.clone(),
        generation: app.requested_review_generation,
        project_id,
        web_url: web_url.clone(),
    };
    assert!(
        app.requested_review_comment_fetches
            .insert(current_generation_fetch_key.clone())
    );
    app.apply_app_events(AppEvent::RequestedReviewCommentSnapshotLoaded {
        display_id,
        generation: stale_generation,
        project_id,
        result: Ok(review_comment_snapshot()),
        web_url,
    })
    .await;

    // Assert
    assert!(
        app.requested_review_comment_fetches
            .contains(&current_generation_fetch_key)
    );
    assert!(matches!(
        app.requested_reviews,
        RequestedReviewState::Loaded {
            ref items,
            project_id: loaded_project_id,
        } if loaded_project_id == project_id
            && items.len() == 1
            && items[0].comment_snapshot.is_none()
    ));
}

#[tokio::test]
/// Ensures startup selection prefers active sessions over archive rows.
async fn test_new_prefers_active_session_for_initial_selection() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await;
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
            "gemini-3-flash-preview",
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
            "gemini-3-flash-preview",
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
async fn test_new_returns_error_when_startup_project_upsert_fails() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let (database, pool) = AppRepositories::in_memory_with_pool().await;
    sqlx::query("DROP TABLE project")
        .execute(&pool)
        .await
        .expect("failed to drop project table");

    // Act
    let error = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .err()
    .expect("expected startup project upsert failure");

    // Assert
    assert!(
        error
            .to_string()
            .contains("Failed to persist startup project")
    );
}

#[tokio::test]
async fn test_new_returns_error_when_startup_active_project_persistence_fails() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let (database, pool) = AppRepositories::in_memory_with_pool().await;
    sqlx::query("DROP TABLE setting")
        .execute(&pool)
        .await
        .expect("failed to drop setting table");

    // Act
    let error = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .err()
    .expect("expected startup active project persistence failure");

    // Assert
    assert!(
        error
            .to_string()
            .contains("Failed to store active startup project")
    );
}

#[tokio::test]
async fn test_new_with_clients_falls_back_from_stale_active_project_and_loads_current_sessions() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let agentty_home = temp_dir.path().join("agentty-home");
    let current_project_path = temp_dir.path().join("current-project");
    fs::create_dir_all(&agentty_home).expect("failed to create agentty home");
    fs::create_dir_all(&current_project_path).expect("failed to create current project");
    fs::create_dir_all(current_project_path.join(".git"))
        .expect("failed to create current project git marker");
    let missing_project_path = temp_dir.path().join("missing-project");
    let database = AppRepositories::in_memory().await;
    let current_project_id = database
        .projects()
        .upsert_project(
            &current_project_path.to_string_lossy(),
            Some("main".to_string()),
        )
        .await
        .expect("failed to insert current project");
    let missing_project_id = database
        .projects()
        .upsert_project(
            &missing_project_path.to_string_lossy(),
            Some("missing".to_string()),
        )
        .await
        .expect("failed to insert missing project");
    database
        .settings()
        .set_active_project_id(missing_project_id)
        .await
        .expect("failed to persist stale active project");
    let current_session_id = "session-current";
    let missing_session_id = "session-missing";
    database
        .sessions()
        .insert_session(
            current_session_id,
            "gemini-3-flash-preview",
            "main",
            &Status::Review.to_string(),
            current_project_id,
        )
        .await
        .expect("failed to insert current project session");
    database
        .sessions()
        .insert_session(
            missing_session_id,
            "gemini-3-flash-preview",
            "main",
            &Status::Review.to_string(),
            missing_project_id,
        )
        .await
        .expect("failed to insert stale project session");
    let current_session_folder =
        agentty_home.join(current_session_id.chars().take(8).collect::<String>());
    fs::create_dir_all(current_session_folder.join(SESSION_DATA_DIR))
        .expect("failed to create current session folder");

    // Act
    let app = App::new_with_clients(
        agentty_home.clone(),
        current_project_path.clone(),
        Some("main".to_string()),
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");

    // Assert
    assert_eq!(app.active_project_id(), current_project_id);
    assert_eq!(app.working_dir(), current_project_path.as_path());
    assert_eq!(app.git_branch(), Some("main"));
    assert_eq!(
        app.selected_session().map(|session| session.id.as_str()),
        Some(current_session_id)
    );
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(app.sessions.sessions()[0].id, current_session_id);
    let project_items = app.projects.render_parts().project_items;
    assert!(
        project_items
            .iter()
            .any(|item| item.project.id == current_project_id)
    );
    assert!(
        !project_items
            .iter()
            .any(|item| item.project.id == missing_project_id)
    );
}

#[tokio::test]
async fn test_new_with_clients_restores_persisted_active_tab() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let agentty_home = temp_dir.path().join("agentty-home");
    let project_path = temp_dir.path().join("project");
    fs::create_dir_all(&agentty_home).expect("failed to create agentty home");
    fs::create_dir_all(project_path.join(".git")).expect("failed to create project git marker");
    let database = AppRepositories::in_memory().await;
    database
        .settings()
        .upsert_setting(SettingName::ActiveTab, Tab::Review.as_str())
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
    assert_eq!(app.tabs.current(), Tab::Review);
}

#[tokio::test]
async fn test_new_with_clients_defaults_to_sessions_when_active_project_exists() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let agentty_home = temp_dir.path().join("agentty-home");
    let project_path = temp_dir.path().join("project");
    fs::create_dir_all(&agentty_home).expect("failed to create agentty home");
    fs::create_dir_all(project_path.join(".git")).expect("failed to create project git marker");
    let database = AppRepositories::in_memory().await;
    let project_id = database
        .projects()
        .upsert_project(&project_path.to_string_lossy(), Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .settings()
        .set_active_project_id(project_id)
        .await
        .expect("failed to persist active project");

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
    assert_eq!(app.tabs.current(), Tab::Sessions);
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

/// Builds a test app with one selected session, configurable launch
/// configuration, and injected tmux boundary.
async fn new_test_app_with_selected_session(
    session_folder: PathBuf,
    launch_configuration: &str,
    tmux_client: Arc<dyn TmuxClient>,
) -> App {
    // Arrange
    let mut app =
        crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(tmux_client)
            .await;
    if !session_folder.as_os_str().is_empty() {
        std::fs::create_dir_all(&session_folder).expect("failed to create session folder");
    }

    // Act
    app.settings.launch_configuration = launch_configuration.to_string();
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            session_folder,
        ));
    app.sessions.select_session_index(Some(0));

    // Assert
    app
}

#[test]
fn branch_publish_popup_helpers_format_copy() {
    // Arrange
    let expected_restore_view = ConfirmationViewMode {
        scroll_offset: Some(2),
        session_id: "session-1".into(),
    };

    // Act
    let loading_title = App::branch_publish_loading_title(PublishBranchAction::Push);
    let loading_message = App::branch_publish_loading_message(PublishBranchAction::Push, None);
    let custom_loading_message = App::branch_publish_loading_message(
        PublishBranchAction::Push,
        Some("review/custom-branch"),
    );
    let loading_label = App::branch_publish_loading_label(PublishBranchAction::Push);
    let success_title = App::branch_publish_success_title(PublishBranchAction::Push);
    let success_message = App::branch_publish_success_message(
        "wt/session-1",
        Some(&crate::app::branch_publish::ReviewRequestCreationInfo {
            forge_kind: forge::ForgeKind::GitHub,
            web_url: Some(
                "https://github.com/org/repo/compare/main...wt%2Fsession-1?expand=1".to_string(),
            ),
        }),
    );
    let fallback_success_message = App::branch_publish_success_message("wt/session-1", None);
    let pull_request_loading_title =
        App::branch_publish_loading_title(PublishBranchAction::PublishPullRequest);
    let pull_request_loading_message =
        App::branch_publish_loading_message(PublishBranchAction::PublishPullRequest, None);
    let pull_request_loading_label =
        App::branch_publish_loading_label(PublishBranchAction::PublishPullRequest);
    let pull_request_success_title =
        App::branch_publish_success_title(PublishBranchAction::PublishPullRequest);
    let popup_mode = App::view_info_popup_mode(
        "Working".to_string(),
        "Publishing branch".to_string(),
        true,
        "Pushing branch...".to_string(),
        expected_restore_view.clone(),
    );

    // Assert
    assert_eq!(loading_title, "Pushing branch");
    assert_eq!(
        loading_message,
        "Publishing the session branch to the configured Git remote."
    );
    assert_eq!(
        custom_loading_message,
        "Publishing the session branch to `review/custom-branch` on the configured Git remote."
    );
    assert_eq!(loading_label, "Pushing branch...");
    assert_eq!(success_title, "Branch pushed");
    assert!(success_message.contains("Pushed session branch `wt/session-1`."));
    assert!(success_message.contains("Open this link to create the pull request"));
    assert!(
        success_message
            .contains("https://github.com/org/repo/compare/main...wt%2Fsession-1?expand=1")
    );
    assert!(fallback_success_message.contains("Create the review request manually"));
    assert_eq!(pull_request_loading_title, "Publishing review request");
    assert_eq!(
        pull_request_loading_message,
        "Pushing the session branch and creating or refreshing the active forge review request."
    );
    assert_eq!(pull_request_loading_label, "Publishing review request...");
    assert_eq!(pull_request_success_title, "Review request published");
    assert!(matches!(
        popup_mode,
        AppMode::ViewInfoPopup {
            is_loading: true,
            ref loading_label,
            ref message,
            ref restore_view,
            ref title,
        } if title == "Working"
            && message == "Publishing branch"
            && loading_label == "Pushing branch..."
            && restore_view == &expected_restore_view
    ));
}

/// Verifies generic and authentication-related branch-push failures map
/// to the correct popup severity and current recovery guidance.
#[test]
fn branch_push_failure_maps_blocked_and_failed_errors() {
    // Arrange
    let auth_error = "Git push failed: fatal: could not read Username for 'https://github.com': \
                      terminal prompts disabled";
    let failed_error = "remote rejected";

    // Act
    let blocked = branch_push_failure(PublishBranchAction::Push, auth_error);
    let failed = branch_push_failure(PublishBranchAction::Push, failed_error);

    // Assert
    assert_eq!(blocked.title, "Branch push blocked");
    assert!(blocked.message.contains("Git push requires authentication"));
    assert!(blocked.message.contains("gh auth login"));
    assert_eq!(failed.title, "Branch push failed");
    assert!(
        failed
            .message
            .contains("Failed to publish session branch: remote rejected")
    );
}

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
    let database = crate::infra::db::AppRepositories::in_memory().await;

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
    let database = crate::infra::db::AppRepositories::in_memory().await;

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
    let database = crate::infra::db::AppRepositories::in_memory().await;

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
async fn branch_publish_task_helpers_reject_unsupported_session_states() {
    // Arrange
    let app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut review_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/review-session"));
    review_session.status = Status::Done;
    let done_snapshot = BranchPublishTaskSession::from_session(&review_session);

    // Act
    let push_result = run_branch_publish_action(
        PublishBranchAction::Push,
        done_snapshot.clone(),
        app.services.db().clone(),
        app.services.clock(),
        app.services.git_client(),
        app.services.review_request_client(),
        None,
    )
    .await;
    let helper_result = push_session_branch(
        PublishBranchAction::Push,
        &done_snapshot,
        app.services.db().clone(),
        app.services.git_client(),
        None,
    )
    .await;

    // Assert
    assert_eq!(
        push_result,
        Err(BranchPublishTaskFailure::failed(
            PublishBranchAction::Push,
            "Session must be in review to push the branch.".to_string(),
        ))
    );
    assert_eq!(
        helper_result,
        Err(BranchPublishTaskFailure::failed(
            PublishBranchAction::Push,
            "Session must be in review to push the branch.".to_string(),
        ))
    );
}

#[tokio::test]
async fn branch_publish_task_session_targets_stacked_parent_review_source_branch() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut parent_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/parent-session"));
    parent_session.id = "parent-session".into();
    parent_session.review_request = Some(ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#12".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "review/parent-session".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("Draft".to_string()),
            target_branch: "main".to_string(),
            title: "Parent review".to_string(),
            web_url: "https://github.com/org/repo/pull/12".to_string(),
        },
    });
    let mut child_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/child-session"));
    child_session.id = "child-session".into();
    child_session.base_branch = session::session_branch("parent-session");
    child_session.parent_session_id = Some("parent-session".into());
    app.sessions.push_session(parent_session);
    app.sessions.push_session(child_session);

    // Act
    let branch_publish_session = app
        .branch_publish_task_session("child-session")
        .expect("expected branch-publish snapshot");

    // Assert
    assert_eq!(branch_publish_session.base_branch, "review/parent-session");
}

#[tokio::test]
async fn branch_publish_task_session_targets_stacked_parent_upstream_branch() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut parent_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/parent-session"));
    parent_session.id = "parent-session".into();
    parent_session.published_upstream_ref = Some("origin/review/parent-custom".to_string());
    let mut child_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/child-session"));
    child_session.id = "child-session".into();
    child_session.base_branch = session::session_branch("parent-session");
    child_session.parent_session_id = Some("parent-session".into());
    app.sessions.push_session(parent_session);
    app.sessions.push_session(child_session);

    // Act
    let branch_publish_session = app
        .branch_publish_task_session("child-session")
        .expect("expected branch-publish snapshot");

    // Assert
    assert_eq!(branch_publish_session.base_branch, "review/parent-custom");
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
        .expect_push_current_branch_to_remote_branch()
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
    let database = crate::infra::db::AppRepositories::in_memory().await;

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
async fn push_session_branch_succeeds_without_review_request_link_for_unsupported_remote() {
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
        .with(
            mockall::predicate::eq(PathBuf::from("/tmp/review-session")),
            mockall::predicate::eq(session::session_branch("session-1")),
        )
        .once()
        .returning(|_, _| Box::pin(async { Ok("origin/wt/session-1".to_string()) }));
    mock_git_client
        .expect_repo_url()
        .with(mockall::predicate::eq(PathBuf::from("/tmp/review-session")))
        .once()
        .returning(|_| Box::pin(async { Ok("https://example.com/team/project.git".to_string()) }));
    let git_client: Arc<dyn ag_git::GitClient> = Arc::new(mock_git_client);
    let database = crate::infra::db::AppRepositories::in_memory().await;

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
    assert_eq!(
        result,
        Ok(BranchPublishTaskSuccess::Pushed {
            branch_name: session::session_branch("session-1"),
            review_request_creation: None,
            upstream_reference: "origin/wt/session-1".to_string(),
        })
    );
}

#[tokio::test]
async fn apply_branch_publish_action_update_sets_success_popup() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let expected_restore_view = ConfirmationViewMode {
        scroll_offset: Some(1),
        session_id: "session-1".into(),
    };

    // Act
    app.apply_branch_publish_action_update(BranchPublishActionUpdate {
        restore_view: expected_restore_view.clone(),
        result: Ok(BranchPublishTaskSuccess::Pushed {
            branch_name: "wt/session-1".to_string(),
            review_request_creation: Some(crate::app::branch_publish::ReviewRequestCreationInfo {
                forge_kind: forge::ForgeKind::GitHub,
                web_url: Some(
                    "https://github.com/agentty-xyz/agentty/compare/main...wt%2Fsession-1?expand=1"
                        .to_string(),
                ),
            }),
            upstream_reference: "origin/wt/session-1".to_string(),
        }),
        session_id: "session-1".into(),
    });

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::ViewInfoPopup {
            is_loading: false,
            ref message,
            ref restore_view,
            ref title,
            ..
        } if title == "Branch pushed"
            && message.contains("Pushed session branch `wt/session-1`.")
            && message.contains("https://github.com/agentty-xyz/agentty/compare/main...wt%2Fsession-1?expand=1")
            && restore_view == &expected_restore_view
    ));
    assert_eq!(
        app.sessions
            .state()
            .sessions()
            .first()
            .and_then(|session| session.published_upstream_ref.as_deref()),
        Some("origin/wt/session-1")
    );
}

#[tokio::test]
async fn apply_branch_publish_action_update_sets_pull_request_success_popup() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let expected_restore_view = ConfirmationViewMode {
        scroll_offset: Some(1),
        session_id: "session-1".into(),
    };
    let review_request = crate::domain::session::ReviewRequest {
        last_refreshed_at: 55,
        summary: crate::domain::session::ReviewRequestSummary {
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            ..test_review_request_summary("#42", ReviewRequestState::Open)
        },
    };

    // Act
    app.apply_branch_publish_action_update(BranchPublishActionUpdate {
        restore_view: expected_restore_view.clone(),
        result: Ok(BranchPublishTaskSuccess::PullRequestPublished {
            branch_name: "wt/session-1".to_string(),
            review_request: review_request.clone(),
            upstream_reference: "origin/wt/session-1".to_string(),
        }),
        session_id: "session-1".into(),
    });

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::ViewInfoPopup {
            is_loading: false,
            ref message,
            ref restore_view,
            ref title,
            ..
        } if title == "GitHub pull request published"
            && message.contains("Published session branch `wt/session-1`.")
            && message.contains("GitHub pull request #42 is ready")
            && message.contains("https://github.com/agentty-xyz/agentty/pull/42")
            && restore_view == &expected_restore_view
    ));
    assert_eq!(
        app.sessions
            .state()
            .sessions()
            .first()
            .and_then(|session| session.review_request.clone()),
        Some(review_request)
    );
}

#[tokio::test]
async fn apply_branch_publish_action_update_sets_gitlab_merge_request_success_popup() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let expected_restore_view = ConfirmationViewMode {
        scroll_offset: Some(2),
        session_id: "session-1".into(),
    };
    let review_request = crate::domain::session::ReviewRequest {
        last_refreshed_at: 77,
        summary: crate::domain::session::ReviewRequestSummary {
            display_id: "!24".to_string(),
            forge_kind: ForgeKind::GitLab,
            source_branch: "wt/session-1".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("Draft".to_string()),
            target_branch: "main".to_string(),
            title: "Add GitLab support".to_string(),
            web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/24".to_string(),
        },
    };

    // Act
    app.apply_branch_publish_action_update(BranchPublishActionUpdate {
        restore_view: expected_restore_view.clone(),
        result: Ok(BranchPublishTaskSuccess::PullRequestPublished {
            branch_name: "wt/session-1".to_string(),
            review_request,
            upstream_reference: "origin/wt/session-1".to_string(),
        }),
        session_id: "session-1".into(),
    });

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::ViewInfoPopup {
            is_loading: false,
            ref message,
            ref restore_view,
            ref title,
            ..
        } if title == "GitLab merge request published"
            && message.contains("Published session branch `wt/session-1`.")
            && message.contains("GitLab merge request !24 is ready")
            && message.contains("https://gitlab.com/agentty-xyz/agentty/-/merge_requests/24")
            && restore_view == &expected_restore_view
    ));
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
    let app = new_test_app_with_selected_session(
        PathBuf::from("/tmp/session-launch-configuration"),
        "  npm run dev  ",
        Arc::new(mock_tmux_client),
    )
    .await;

    // Act
    app.open_session_worktree_in_tmux().await;

    // Assert
    // Expectations are validated by `mockall`.
}

#[tokio::test]
async fn open_session_worktree_in_tmux_skips_launch_configuration_when_setting_is_blank() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session-empty-launch-configuration");
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client
        .expect_open_window_for_folder()
        .with(eq(session_folder))
        .times(1)
        .returning(|_| Box::pin(async { Some("@42".to_string()) }));
    mock_tmux_client.expect_run_command_in_window().times(0);
    let app = new_test_app_with_selected_session(
        PathBuf::from("/tmp/session-empty-launch-configuration"),
        "   ",
        Arc::new(mock_tmux_client),
    )
    .await;

    // Act
    app.open_session_worktree_in_tmux().await;

    // Assert
    // Expectations are validated by `mockall`.
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
    let app = new_test_app_with_selected_session(
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

#[test]
fn sync_main_popup_mode_success_message_tracks_project_and_branch() {
    // Arrange
    let sync_popup_context = SyncPopupContext {
        default_branch: "develop".to_string(),
        project_name: "agentty".to_string(),
    };
    let sync_main_outcome = SyncMainOutcome {
        pulled_commit_titles: vec![
            "Add audit log indexing".to_string(),
            "Fix merge conflict prompt wording".to_string(),
        ],
        pulled_commits: Some(2),
        pushed_commit_titles: vec!["Polish sync popup alignment".to_string()],
        pushed_commits: Some(1),
        resolved_conflict_files: vec!["src/lib.rs".to_string()],
    };

    // Act
    let mode = App::sync_main_popup_mode(Ok(sync_main_outcome), &sync_popup_context);
    let expected_message = concat!(
        "Successfully synchronized with its upstream.\n",
        "\n",
        "## 1. 2 commits pulled\n",
        "  - Add audit log indexing\n",
        "  - Fix merge conflict prompt wording\n",
        "\n",
        "## 2. 1 commit pushed\n",
        "  - Polish sync popup alignment\n",
        "\n",
        "## 3. conflicts fixed: src/lib.rs"
    );

    // Assert
    assert!(matches!(mode, AppMode::SyncBlockedPopup { .. }));
    if let AppMode::SyncBlockedPopup {
        default_branch,
        is_loading,
        message,
        project_name,
        title,
    } = mode
    {
        assert_eq!(title, "Sync complete");
        assert_eq!(default_branch.as_deref(), Some("develop"));
        assert!(!is_loading);
        assert_eq!(message, expected_message);
        assert_eq!(project_name.as_deref(), Some("agentty"));
    }
}

#[tokio::test]
async fn apply_app_events_sync_conflicts_updates_loading_popup() {
    // Arrange
    let (mut app, base_dir) = crate::test_support::new_test_app().await;
    let expected_project_name = base_dir
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .expect("expected temp dir file name")
        .to_string();
    app.projects.update_active_project_context(
        app.active_project_id(),
        expected_project_name.clone(),
        Some("develop".to_string()),
        None,
        base_dir.path().to_path_buf(),
    );
    app.mode = AppMode::SyncBlockedPopup {
        default_branch: Some("develop".to_string()),
        is_loading: true,
        message: App::sync_loading_message(),
        project_name: Some(expected_project_name.clone()),
        title: "Sync in progress".to_string(),
    };

    // Act
    app.apply_app_events(AppEvent::SyncMainConflictResolutionStarted {
        conflicted_files: vec!["src/lib.rs".to_string(), "README.md".to_string()],
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::SyncBlockedPopup {
            ref default_branch,
            is_loading: true,
            ref message,
            ref project_name,
            ref title,
        } if title == "Resolving conflicts"
            && default_branch.as_deref() == Some("develop")
            && project_name.as_deref() == Some(expected_project_name.as_str())
            && message.contains("Resolving conflicts during sync.")
            && message.contains("- README.md")
            && message.contains("- src/lib.rs")
    ));
}

#[test]
fn sync_main_popup_mode_blocked_message_tracks_project_and_branch() {
    // Arrange
    let sync_popup_context = SyncPopupContext {
        default_branch: "develop".to_string(),
        project_name: "agentty".to_string(),
    };

    // Act
    let mode = App::sync_main_popup_mode(
        Err(SyncSessionStartError::MainHasUncommittedChanges {
            default_branch: "develop".to_string(),
        }),
        &sync_popup_context,
    );

    // Assert
    assert!(matches!(
        mode,
        AppMode::SyncBlockedPopup {
            ref default_branch,
            is_loading: false,
            ref title,
            ref message,
            ref project_name,
        } if title == "Sync blocked"
            && default_branch.as_deref() == Some("develop")
            && message.contains("uncommitted changes")
            && project_name.as_deref() == Some("agentty")
    ));
}

#[test]
fn sync_main_popup_mode_auth_failure_shows_authorization_guidance() {
    // Arrange
    let sync_popup_context = SyncPopupContext {
        default_branch: "main".to_string(),
        project_name: "agentty".to_string(),
    };
    let sync_error = SyncSessionStartError::Other(
        "Git push failed: fatal: could not read Username for 'https://github.com': terminal \
         prompts disabled"
            .to_string(),
    );

    // Act
    let mode = App::sync_main_popup_mode(Err(sync_error), &sync_popup_context);

    // Assert
    assert!(matches!(
        mode,
        AppMode::SyncBlockedPopup {
            ref default_branch,
            is_loading: false,
            ref title,
            ref message,
            ref project_name,
        } if title == "Sync failed"
            && default_branch.as_deref() == Some("main")
            && message.contains("Git push requires authentication")
            && message.contains("`gh auth login`")
            && message.contains("then run sync again")
            && project_name.as_deref() == Some("agentty")
    ));
}

#[test]
fn sync_push_auth_error_detects_github_from_prompt_url() {
    // Arrange
    let detail =
        "Git push failed: fatal: could not read Password for 'https://github.com/team/project': \
         terminal prompts disabled\nConfigured remotes:\n  github.com";

    // Act
    let forge_kind = detected_forge_kind_from_git_push_error(detail);

    // Assert
    assert_eq!(forge_kind, Some(forge::ForgeKind::GitHub));
}

#[test]
fn sync_push_auth_error_prefers_github_when_fallback_markers_are_ambiguous() {
    // Arrange
    let detail = "Git push failed: authentication failed. Configure remotes:\n  github.com";

    // Act
    let forge_kind = detected_forge_kind_from_git_push_error(detail);

    // Assert
    assert_eq!(forge_kind, Some(forge::ForgeKind::GitHub));
}

#[test]
fn app_event_batch_collect_event_keeps_latest_at_mention_entries_update() {
    // Arrange
    let mut event_batch = AppEventBatch::default();
    let first_entries = vec![FileEntry {
        is_dir: false,
        path: "src/main.rs".to_string(),
    }];
    let second_entries = vec![FileEntry {
        is_dir: true,
        path: "crates".to_string(),
    }];

    // Act
    event_batch.collect_event(AppEvent::AtMentionEntriesLoaded {
        entries: first_entries,
        session_id: "session-1".into(),
    });
    event_batch.collect_event(AppEvent::AtMentionEntriesLoaded {
        entries: second_entries.clone(),
        session_id: "session-1".into(),
    });

    // Assert
    assert_eq!(
        event_batch
            .at_mention_entries_updates
            .get("session-1")
            .cloned(),
        Some(second_entries)
    );
}

#[test]
/// Verifies repeated `AgentResponseReceived` events keep the newest
/// reducer projection while accumulating token usage for the session.
fn app_event_batch_collect_event_merges_agent_response_token_usage() {
    // Arrange
    let mut event_batch = AppEventBatch::default();
    let latest_turn = test_turn_applied_state(
        vec![
            QuestionItem::new("Need branch?"),
            QuestionItem::new("Need tests?"),
        ],
        vec!["Document the batched reducer path."],
        Some(AgentResponseSummary {
            session: "Session summary".to_string(),
            turn: "Latest turn summary".to_string(),
        }),
        SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            input_tokens: 7,
            output_tokens: 11,
        },
    );

    // Act
    event_batch.collect_event(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: test_turn_applied_state(
            vec![QuestionItem::new("Old question")],
            vec!["Old follow-up task"],
            Some(AgentResponseSummary {
                session: "Old session summary".to_string(),
                turn: "Old turn summary".to_string(),
            }),
            SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                input_tokens: 3,
                output_tokens: 5,
            },
        ),
    });
    event_batch.collect_event(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: latest_turn.clone(),
    });

    // Assert
    let merged_turn = event_batch.applied_turns.get("session-1");
    assert_eq!(
        merged_turn.map(|turn| &turn.questions),
        Some(&latest_turn.questions)
    );
    assert_eq!(
        merged_turn.map(|turn| {
            turn.follow_up_tasks
                .iter()
                .map(|task| task.text.clone())
                .collect::<Vec<_>>()
        }),
        Some(vec!["Document the batched reducer path.".to_string()])
    );
    assert_eq!(
        merged_turn.map(|turn| turn.token_usage_delta.input_tokens),
        Some(10)
    );
    assert_eq!(
        merged_turn.map(|turn| turn.token_usage_delta.output_tokens),
        Some(16)
    );
}

#[test]
/// Verifies that `UpdateStatusChanged` events update the event batch so
/// the reducer can apply the latest update progress state.
fn app_event_batch_collect_event_stores_update_status() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::UpdateStatusChanged {
        update_status: UpdateStatus::InProgress {
            version: "v1.0.0".to_string(),
        },
    });
    event_batch.collect_event(AppEvent::UpdateStatusChanged {
        update_status: UpdateStatus::Complete {
            version: "v1.0.0".to_string(),
        },
    });

    // Assert
    assert_eq!(
        event_batch.update_status,
        Some(UpdateStatus::Complete {
            version: "v1.0.0".to_string()
        })
    );
}

#[test]
/// Verifies that `AgentCliVersionsUpdated` events keep the latest
/// completed version snapshot in one reducer batch.
fn app_event_batch_collect_event_stores_agent_cli_versions() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::AgentCliVersionsUpdated {
        agent_clis: vec![AgentCliInfo::new(
            AgentKind::Claude,
            Some("2.1.39".to_string()),
        )],
    });
    event_batch.collect_event(AppEvent::AgentCliVersionsUpdated {
        agent_clis: vec![AgentCliInfo::new(
            AgentKind::Codex,
            Some("0.139.0".to_string()),
        )],
    });

    // Assert
    assert_eq!(
        event_batch.agent_cli_updates,
        Some(vec![AgentCliInfo::new(
            AgentKind::Codex,
            Some("0.139.0".to_string())
        )])
    );
}

/// Verifies system log events are retained in reducer batch order.
#[test]
fn app_event_batch_collect_event_keeps_system_log_events_ordered() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::SystemLog {
        event: SystemLogEvent::new(
            SystemLogLevel::Info,
            SystemLogCategory::System,
            "First event",
        ),
    });
    event_batch.collect_event(AppEvent::SystemLog {
        event: SystemLogEvent::new(
            SystemLogLevel::Warning,
            SystemLogCategory::Forge,
            "Second event",
        ),
    });

    // Assert
    assert_eq!(event_batch.system_log_events.len(), 2);
    assert_eq!(event_batch.system_log_events[0].message, "First event");
    assert_eq!(event_batch.system_log_events[1].message, "Second event");
}

#[test]
fn app_event_batch_collect_event_keeps_latest_same_session_updates() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::SessionModelUpdated {
        session_id: "session-a".into(),
        session_agent: AgentSelection::new(AgentKind::Gemini, AgentModel::Gemini3FlashPreview),
    });
    event_batch.collect_event(AppEvent::SessionModelUpdated {
        session_id: "session-a".into(),
        session_agent: AgentSelection::new(AgentKind::Gemini, AgentModel::Gemini31ProPreview),
    });
    event_batch.collect_event(AppEvent::SessionProgressUpdated {
        progress_message: Some("first".to_string()),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::SessionProgressUpdated {
        progress_message: Some("second".to_string()),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::SessionSizeUpdated {
        added_lines: 1,
        deleted_lines: 2,
        session_id: "session-a".into(),
        session_size: SessionSize::S,
    });
    event_batch.collect_event(AppEvent::SessionSizeUpdated {
        added_lines: 8,
        deleted_lines: 13,
        session_id: "session-a".into(),
        session_size: SessionSize::L,
    });
    event_batch.collect_event(AppEvent::SessionTitleGenerationFinished {
        generation: 1,
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::SessionTitleGenerationFinished {
        generation: 2,
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::SessionUpdated {
        session_id: "session-a".into(),
        version: 1,
    });
    event_batch.collect_event(AppEvent::SessionUpdated {
        session_id: "session-a".into(),
        version: 2,
    });
    event_batch.collect_event(AppEvent::AgentResponseReceived {
        session_id: "session-a".into(),
        turn_applied_state: test_turn_applied_state(
            vec![QuestionItem::new("first question")],
            Vec::new(),
            None,
            SessionStats::default(),
        ),
    });
    event_batch.collect_event(AppEvent::AgentResponseReceived {
        session_id: "session-a".into(),
        turn_applied_state: test_turn_applied_state(
            vec![QuestionItem::new("second question")],
            Vec::new(),
            None,
            SessionStats::default(),
        ),
    });

    // Assert
    assert_eq!(
        event_batch.session_model_updates.get("session-a"),
        Some(&AgentSelection::new(
            AgentKind::Gemini,
            AgentModel::Gemini31ProPreview
        ))
    );
    assert_eq!(
        event_batch.session_progress_updates.get("session-a"),
        Some(&Some("second".to_string()))
    );
    assert_eq!(
        event_batch.session_size_updates.get("session-a"),
        Some(&(8, 13, SessionSize::L))
    );
    assert_eq!(
        event_batch.session_update_versions.get("session-a"),
        Some(&2)
    );
    assert_eq!(
        event_batch
            .session_title_generation_finished
            .get("session-a"),
        Some(&2)
    );
    assert_eq!(event_batch.session_ids.len(), 1);
    assert_eq!(
        event_batch
            .applied_turns
            .get("session-a")
            .map(|turn_applied_state| turn_applied_state.questions.clone()),
        Some(vec![QuestionItem::new("second question")])
    );
}

#[test]
fn app_event_batch_collect_event_preserves_reviews_and_uses_final_branch_publish() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::ReviewPrepared {
        diff_hash: 11,
        review_text: "first review".to_string(),
        session_id: "session-a".into(),
        turn_id: 1,
    });
    event_batch.collect_event(AppEvent::ReviewPreparationFailed {
        diff_hash: 12,
        error: "latest failure".to_string(),
        session_id: "session-a".into(),
        turn_id: 2,
    });
    event_batch.collect_event(AppEvent::ReviewPrepared {
        diff_hash: 21,
        review_text: "stable review".to_string(),
        session_id: "session-b".into(),
        turn_id: 3,
    });
    event_batch.collect_event(AppEvent::BranchPublishActionCompleted {
        restore_view: test_confirmation_view_mode("session-a"),
        result: Box::new(Ok(test_pushed_branch_result("feature/first"))),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::BranchPublishActionCompleted {
        restore_view: test_confirmation_view_mode("session-b"),
        result: Box::new(Ok(test_pushed_branch_result("feature/final"))),
        session_id: "session-b".into(),
    });

    // Assert
    assert_eq!(
        event_batch.review_updates,
        vec![
            (
                "session-a".into(),
                ReviewUpdate {
                    diff_hash: 11,
                    result: Ok("first review".to_string()),
                    turn_id: 1,
                },
            ),
            (
                "session-a".into(),
                ReviewUpdate {
                    diff_hash: 12,
                    result: Err("latest failure".to_string()),
                    turn_id: 2,
                },
            ),
            (
                "session-b".into(),
                ReviewUpdate {
                    diff_hash: 21,
                    result: Ok("stable review".to_string()),
                    turn_id: 3,
                },
            ),
        ]
    );
    assert_eq!(
        event_batch.branch_publish_action_update,
        Some(BranchPublishActionUpdate {
            restore_view: test_confirmation_view_mode("session-b"),
            result: Ok(test_pushed_branch_result("feature/final")),
            session_id: "session-b".into(),
        })
    );
    assert!(event_batch.should_refresh_git_status);
}

#[test]
/// Verifies successful sync completion requests an immediate git-status
/// refresh in the reducer batch.
fn app_event_batch_collect_event_marks_successful_sync_for_git_status_refresh() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::SyncMainCompleted {
        result: Ok(SyncMainOutcome {
            pulled_commit_titles: vec!["Upstream fix".to_string()],
            pulled_commits: Some(1),
            pushed_commit_titles: vec!["Local tweak".to_string()],
            pushed_commits: Some(2),
            resolved_conflict_files: Vec::new(),
        }),
    });

    // Assert
    assert!(event_batch.should_refresh_git_status);
    assert!(matches!(
        event_batch.sync_main_result,
        Some(Ok(SyncMainOutcome {
            pulled_commits: Some(1),
            pushed_commits: Some(2),
            ..
        }))
    ));
}

#[tokio::test]
/// Verifies that the reducer applies `UpdateStatusChanged` events to
/// `App.update_status`.
async fn apply_app_events_update_status_changed_updates_app_state() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    assert!(app.update_status().is_none());
    app.clear_redraw();

    // Act
    app.apply_app_events(AppEvent::UpdateStatusChanged {
        update_status: UpdateStatus::InProgress {
            version: "v2.0.0".to_string(),
        },
    })
    .await;

    // Assert
    assert_eq!(
        app.update_status().cloned(),
        Some(UpdateStatus::InProgress {
            version: "v2.0.0".to_string()
        })
    );
    assert!(app.needs_redraw());
}

#[tokio::test]
/// Verifies that completed CLI version events replace startup loading
/// rows and request a redraw.
async fn apply_app_events_agent_cli_versions_updates_app_services() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.services
        .replace_available_agent_clis(vec![AgentCliInfo::loading(AgentKind::Claude)]);
    app.clear_redraw();

    // Act
    app.apply_app_events(AppEvent::AgentCliVersionsUpdated {
        agent_clis: vec![AgentCliInfo::new(
            AgentKind::Claude,
            Some("2.1.39".to_string()),
        )],
    })
    .await;

    // Assert
    assert_eq!(
        app.services.available_agent_clis(),
        vec![AgentCliInfo::new(
            AgentKind::Claude,
            Some("2.1.39".to_string())
        )]
    );
    assert!(app.needs_redraw());
}

/// Verifies direct system log events append to the process-local buffer.
#[tokio::test]
async fn apply_app_events_records_system_log_events() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let initial_log_count = app.system_logs.len();
    app.clear_redraw();

    // Act
    app.apply_app_events(AppEvent::SystemLog {
        event: SystemLogEvent::new(
            SystemLogLevel::Success,
            SystemLogCategory::Sync,
            "Manual sync completed",
        )
        .with_detail("main"),
    })
    .await;

    // Assert
    assert_eq!(app.system_logs.len(), initial_log_count + 1);
    let entry = app
        .system_logs
        .entries()
        .back()
        .expect("system log event should be recorded");
    assert_eq!(entry.level, SystemLogLevel::Success);
    assert_eq!(entry.category, SystemLogCategory::Sync);
    assert_eq!(entry.message, "Manual sync completed");
    assert_eq!(entry.detail.as_deref(), Some("main"));
    assert!(app.needs_redraw());
}

#[tokio::test]
/// Verifies workflow notices append to the session timeline in event order.
async fn apply_app_events_session_workflow_notice_updates_session_state() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-review"));
    session.id = "session-1".into();
    let transcript = crate::test_support::assistant_transcript("assistant output");
    session.transcript = Some(transcript.clone());
    app.sessions.push_session(session);
    app.sessions.session_handles_mut().insert(
        "session-1".into(),
        SessionHandles::new_with_transcript(Status::Review, transcript),
    );
    app.services
        .event_sender()
        .send(AppEvent::SessionWorkflowNoticeUpdated {
            notice: "[Merge] Successfully merged wt/session-1 into main".to_string(),
            session_id: "session-1".into(),
        })
        .expect("queued workflow notice should send");
    app.clear_redraw();

    // Act
    app.apply_app_events(AppEvent::SessionWorkflowNoticeUpdated {
        notice: "[Commit] No changes to commit.".to_string(),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == "session-1")
        .expect("session should exist");
    let transcript = session
        .transcript
        .as_ref()
        .and_then(SessionTranscript::replay_text)
        .unwrap_or_default();
    assert!(transcript.contains("assistant output"));
    assert!(transcript.contains("[Commit] No changes to commit."));
    assert!(transcript.contains("[Merge] Successfully merged"));
    assert!(app.needs_redraw());
}

#[tokio::test]
/// Verifies stale `SessionUpdated` versions do not re-arm redraw when the
/// reducer has already applied that handle snapshot.
async fn apply_app_events_session_updated_same_version_keeps_redraw_clean() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;

    // Act
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: "session-1".into(),
        version: 7,
    })
    .await;
    app.clear_redraw();
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: "session-1".into(),
        version: 7,
    })
    .await;

    // Assert
    assert!(!app.needs_redraw());
}

#[tokio::test]
/// Verifies that one combined git-status event updates the in-memory
/// session snapshot cache.
async fn apply_app_events_git_status_updated_updates_project_and_session_state() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-git-status"),
        ));

    // Act
    app.apply_app_events(AppEvent::GitStatusUpdated {
        generation: app.sync_handle.current_generation(),
        session_statuses: HashMap::from([(
            SessionId::from("session-1"),
            SessionGitStatus {
                base_status: Some((4, 2)),
                remote_status: Some((1, 0)),
            },
        )]),
        status: Some((1, 3)),
    })
    .await;

    // Assert
    assert_eq!(app.git_status_info(), Some((1, 3)));
    assert_eq!(
        app.sessions
            .render_parts()
            .session_git_statuses
            .get("session-1"),
        Some(&SessionGitStatus {
            base_status: Some((4, 2)),
            remote_status: Some((1, 0)),
        })
    );
}

#[tokio::test]
/// Verifies stale git-status snapshots do not overwrite the current sync
/// generation.
async fn apply_app_events_git_status_updated_ignores_stale_generation() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.publish_sync_context_for_refresh();
    let stale_generation = app.sync_handle.current_generation().saturating_sub(1);

    // Act
    app.apply_app_events(AppEvent::GitStatusUpdated {
        generation: stale_generation,
        session_statuses: HashMap::new(),
        status: Some((9, 9)),
    })
    .await;

    // Assert
    assert_eq!(app.git_status_info(), None);
    assert!(app.sessions.render_parts().session_git_statuses.is_empty());
}

#[tokio::test]
/// Verifies stale review-request status results cannot transition a
/// session after the sync context moved to a newer generation.
async fn apply_app_events_review_request_status_updated_ignores_stale_generation() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-review-stale"),
        ));
    app.publish_sync_context_for_refresh();
    let stale_generation = app.sync_handle.current_generation().saturating_sub(1);

    // Act
    app.apply_app_events(AppEvent::ReviewRequestStatusUpdated {
        generation: stale_generation,
        result: Ok(SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::Closed {
                display_id: "#42".to_string(),
            },
            summary: None,
        }),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == "session-1")
        .expect("session should remain loaded");
    assert_eq!(session.status, Status::Review);
}

/// Verifies reducer-applied session status transitions are recorded in
/// the process-local system log.
#[tokio::test]
async fn apply_app_events_review_request_status_transition_records_system_log() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-log-transition";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            AgentModel::Gemini3FlashPreview.as_str(),
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert session");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    let session_data_dir = app
        .services
        .base_path()
        .join(session_folder_name)
        .join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session data dir");
    app.refresh_sessions_now().await;
    let generation = app.sync_handle.current_generation();
    let initial_log_count = app.system_logs.len();

    // Act
    app.apply_app_events(AppEvent::ReviewRequestStatusUpdated {
        generation,
        result: Ok(SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::Closed {
                display_id: "#42".to_string(),
            },
            summary: None,
        }),
        session_id: session_id.into(),
    })
    .await;
    app.process_pending_app_events().await;

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should remain loaded");
    assert_eq!(session.status, Status::Canceled);
    let status_log_entry = app
        .system_logs
        .entries()
        .iter()
        .skip(initial_log_count)
        .find(|entry| entry.message == "Session status changed")
        .expect("status transition should be logged");
    assert_eq!(status_log_entry.category, SystemLogCategory::Session);
    assert_eq!(status_log_entry.level, SystemLogLevel::Info);
    assert!(
        status_log_entry
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("Review -> Canceled"))
    );
}

#[tokio::test]
/// Verifies review-request status updates emitted before a sync
/// completion in the same reducer batch are applied before the
/// post-sync refresh bumps the status generation.
async fn apply_app_events_review_request_status_survives_same_batch_sync_refresh() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-sync-batch";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            "gemini-3-flash-preview",
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert session");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    let session_data_dir = app
        .services
        .base_path()
        .join(session_folder_name)
        .join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session data dir");
    app.refresh_sessions_now().await;
    let generation = app.sync_handle.current_generation();
    app.services
        .event_sender()
        .send(AppEvent::SyncMainCompleted {
            result: Ok(SyncMainOutcome {
                pulled_commit_titles: Vec::new(),
                pulled_commits: Some(0),
                pushed_commit_titles: Vec::new(),
                pushed_commits: Some(0),
                resolved_conflict_files: Vec::new(),
            }),
        })
        .expect("sync completion should queue");

    // Act
    app.apply_app_events(AppEvent::ReviewRequestStatusUpdated {
        generation,
        result: Ok(SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::Closed {
                display_id: "#42".to_string(),
            },
            summary: None,
        }),
        session_id: session_id.into(),
    })
    .await;
    app.process_pending_app_events().await;

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should remain loaded");
    assert_eq!(session.status, Status::Canceled);
}

#[tokio::test]
/// Verifies explicit git-status refresh events request an immediate
/// orchestrator pass instead of waiting for the periodic cadence.
async fn apply_app_events_refresh_git_status_requests_orchestrator_refresh() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await;
    let clients = crate::test_support::test_app_clients()
        .with_app_server_client_override(crate::test_support::mock_app_server())
        .with_tmux_client(Arc::new(MockTmuxClient::new()));
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path.clone(),
        Some("main".to_string()),
        database,
        clients,
    )
    .await
    .expect("failed to build test app");
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .times(1)
        .returning(|dir| Box::pin(async move { Some(dir) }));
    mock_git_client
        .expect_fetch_remote()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(1)
        .returning(|_| {
            Box::pin(async { Ok(HashMap::from([("main".to_string(), Some((2_u32, 1_u32)))])) })
        });
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.apply_app_events(AppEvent::RefreshGitStatus).await;
    let mut observed_events = vec![
        tokio::time::timeout(Duration::from_secs(1), app.next_app_event())
            .await
            .expect("first app event should arrive")
            .expect("app event channel should remain open"),
    ];
    if !observed_events
        .iter()
        .any(|event| matches!(event, AppEvent::GitStatusUpdated { .. }))
    {
        let next_event = tokio::time::timeout(Duration::from_secs(1), app.next_app_event()).await;
        assert!(
            next_event.is_ok(),
            "git status refresh event should arrive after observed events: {observed_events:?}"
        );
        let next_event = next_event
            .expect("git status refresh timeout should be checked")
            .expect("app event channel should remain open");
        observed_events.push(next_event);
    }

    // Assert
    assert!(
        observed_events.contains(&AppEvent::GitStatusUpdated {
            generation: app.sync_handle.current_generation(),
            session_statuses: HashMap::new(),
            status: Some((2, 1)),
        }),
        "expected git status update among observed events: {observed_events:?}"
    );
}

#[tokio::test]
async fn apply_app_events_agent_response_switches_view_mode_to_question_mode() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-question-view"),
        ));
    app.mode = AppMode::View {
        session_id: "session-1".into(),
        scroll_offset: None,
    };
    let expected_questions = vec![
        QuestionItem::with_options(
            "Need a target branch?",
            vec!["main".to_string(), "develop".to_string()],
        ),
        QuestionItem::with_options(
            "Need integration tests?",
            vec!["Yes".to_string(), "No".to_string()],
        ),
    ];
    let turn_applied_state = test_turn_applied_state(
        vec![
            QuestionItem::with_options(
                "Need a target branch?",
                vec!["main".to_string(), "develop".to_string()],
            ),
            QuestionItem::with_options(
                "Need integration tests?",
                vec!["Yes".to_string(), "No".to_string()],
            ),
        ],
        Vec::new(),
        None,
        SessionStats::default(),
    );

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state,
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question {
            ref session_id,
            ref questions,
            ref responses,
            current_index: 0,
            ref input,
            selected_option_index: Some(0),
            ..
        } if session_id == "session-1"
            && questions == &expected_questions
            && responses.is_empty()
            && input.text().is_empty()
    ));
}

#[tokio::test]
async fn reconcile_open_session_question_mode_enters_question_mode_from_view() {
    // Arrange — a viewed session reached `Question` status with pending
    // questions, but the view was never flipped into the clarification panel
    // (for example the live projection was missed while an overlay was open).
    let pending_questions = vec![
        QuestionItem::with_options("Need a target branch?", vec!["main".to_string()]),
        QuestionItem::new("Need integration tests?"),
    ];
    let mut app = test_app_viewing_reconcile_session(
        Status::Question,
        pending_questions.clone(),
        "session-question-reconcile",
    )
    .await;

    // Act
    app.reconcile_open_session_question_mode().await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question {
            ref session_id,
            questions: ref mode_questions,
            current_index: 0,
            ..
        } if session_id == "session-1" && mode_questions == &pending_questions
    ));
}

#[tokio::test]
async fn reconcile_open_session_question_mode_ignores_non_question_status() {
    // Arrange — the viewed session is in `Review`, not awaiting a question.
    let mut app =
        test_app_viewing_reconcile_session(Status::Review, Vec::new(), "session-review-reconcile")
            .await;

    // Act
    app.reconcile_open_session_question_mode().await;

    // Assert — the view is preserved.
    assert!(matches!(
        app.mode,
        AppMode::View { ref session_id, .. } if session_id == "session-1"
    ));
}

#[tokio::test]
async fn reconcile_open_session_question_mode_ignores_non_view_modes() {
    // Arrange — a `Question` session exists, but the user is on the list, not
    // viewing that session, so the panel must not steal focus.
    let mut app = test_app_viewing_reconcile_session(
        Status::Question,
        vec![QuestionItem::new("Need integration tests?")],
        "session-list-reconcile",
    )
    .await;
    app.mode = AppMode::List;

    // Act
    app.reconcile_open_session_question_mode().await;

    // Assert — the list stays active.
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn reconcile_open_session_question_mode_reloads_detail_at_most_once_when_still_empty() {
    // Arrange — a viewed session reports `Question` status but carries no
    // questions in the snapshot, and no persisted detail exists to reload, so
    // the reconciliation cannot open the panel.
    let mut app =
        test_app_viewing_reconcile_session(Status::Question, Vec::new(), "session-question-empty")
            .await;

    // Act — run two reconciliations to emulate two consecutive render cycles
    // while the session stays stuck without questions.
    app.reconcile_open_session_question_mode().await;
    let attempted_after_first = app.question_reconcile_reload_attempted.clone();
    app.reconcile_open_session_question_mode().await;

    // Assert — the first pass records the stuck session so the second cycle
    // short-circuits before reloading detail again, and the view is preserved
    // because no questions became available.
    assert_eq!(attempted_after_first.as_deref(), Some("session-1"));
    assert_eq!(
        app.question_reconcile_reload_attempted.as_deref(),
        Some("session-1")
    );
    assert!(matches!(
        app.mode,
        AppMode::View { ref session_id, .. } if session_id == "session-1"
    ));
}

#[tokio::test]
async fn reconcile_open_session_question_mode_clears_reload_guard_when_leaving_view() {
    // Arrange — a stuck `Question` view records the reload guard, then the user
    // navigates back to the list.
    let mut app = test_app_viewing_reconcile_session(
        Status::Question,
        Vec::new(),
        "session-question-guard-reset",
    )
    .await;
    app.reconcile_open_session_question_mode().await;
    assert_eq!(
        app.question_reconcile_reload_attempted.as_deref(),
        Some("session-1")
    );

    // Act — leave the session view and reconcile again.
    app.mode = AppMode::List;
    app.reconcile_open_session_question_mode().await;

    // Assert — the guard is cleared so a later legitimate transition reloads.
    assert!(app.question_reconcile_reload_attempted.is_none());
}

#[tokio::test]
async fn apply_app_events_agent_response_clears_saved_question_progress() {
    // Arrange — stale partial answers saved from the previous question set
    // must not survive a new turn result for the session.
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-progress-clear"),
        ));
    app.question_progress.insert(
        "session-1".into(),
        QuestionProgress {
            current_index: 1,
            input: InputState::default(),
            responses: vec!["Old answer".to_string()],
            selected_option_index: None,
        },
    );

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: test_turn_applied_state(
            vec![QuestionItem::new("New question?")],
            Vec::new(),
            None,
            SessionStats::default(),
        ),
    })
    .await;

    // Assert
    assert!(app.question_progress.is_empty());
}

#[tokio::test]
async fn enter_question_mode_restores_saved_progress() {
    // Arrange — progress saved by a previous `q` exit from question mode.
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let questions = vec![
        QuestionItem::with_options("First?", vec!["Yes".to_string(), "No".to_string()]),
        QuestionItem::new("Second?"),
    ];
    app.question_progress.insert(
        "session-restore".into(),
        QuestionProgress {
            current_index: 1,
            input: InputState::with_text("draft answer".to_string()),
            responses: vec!["Yes".to_string()],
            selected_option_index: None,
        },
    );

    // Act
    app.enter_question_mode("session-restore", questions);

    // Assert — resumes at the second question with the saved answer, and
    // the stored entry is consumed.
    assert!(matches!(
        &app.mode,
        AppMode::Question {
            current_index: 1,
            responses,
            input,
            selected_option_index: None,
            session_id,
            ..
        } if responses == &vec!["Yes".to_string()]
            && input.text() == "draft answer"
            && session_id == "session-restore"
    ));
    assert!(app.question_progress.is_empty());
}

#[tokio::test]
async fn enter_question_mode_discards_progress_for_changed_question_list() {
    // Arrange — saved progress no longer matches the question list.
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let questions = vec![QuestionItem::with_options(
        "Only question?",
        vec!["Yes".to_string()],
    )];
    app.question_progress.insert(
        "session-stale".into(),
        QuestionProgress {
            current_index: 2,
            input: InputState::default(),
            responses: vec!["One".to_string(), "Two".to_string()],
            selected_option_index: None,
        },
    );

    // Act
    app.enter_question_mode("session-stale", questions);

    // Assert — starts fresh at the first question with its first option
    // highlighted.
    assert!(matches!(
        &app.mode,
        AppMode::Question {
            current_index: 0,
            responses,
            selected_option_index: Some(0),
            ..
        } if responses.is_empty()
    ));
}

#[tokio::test]
async fn apply_app_events_agent_response_keeps_list_mode_when_not_viewing_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.mode = AppMode::List;

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: test_turn_applied_state(
            vec![QuestionItem::new("Need context?")],
            Vec::new(),
            None,
            SessionStats::default(),
        ),
    })
    .await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
/// Verifies agent responses update cached follow-up tasks immediately for
/// the active session.
async fn apply_app_events_agent_response_updates_session_follow_up_tasks() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-follow-up-view"),
        ));

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: test_turn_applied_state(
            Vec::new(),
            vec![
                "Document the new shortcut.",
                "Add a focused regression test.",
            ],
            None,
            SessionStats::default(),
        ),
    })
    .await;

    // Assert
    assert_eq!(
        app.sessions.sessions()[0]
            .follow_up_tasks
            .iter()
            .map(|task| task.text.clone())
            .collect::<Vec<_>>(),
        vec![
            "Document the new shortcut.".to_string(),
            "Add a focused regression test.".to_string()
        ]
    );
}
#[tokio::test]
/// Verifies reducer-applied turn projections clear stale questions and add
/// token deltas to cached session stats.
async fn apply_app_events_agent_response_updates_questions_and_token_usage() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-stats-view"));
    session.questions = vec![QuestionItem::new("Old question?")];
    session.stats.input_tokens = 5;
    session.stats.output_tokens = 8;
    app.sessions.push_session(session);

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: test_turn_applied_state(
            Vec::new(),
            Vec::new(),
            None,
            SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                input_tokens: 13,
                output_tokens: 21,
            },
        ),
    })
    .await;

    // Assert
    assert!(app.sessions.sessions()[0].questions.is_empty());
    assert_eq!(app.sessions.sessions()[0].stats.input_tokens, 18);
    assert_eq!(app.sessions.sessions()[0].stats.output_tokens, 29);
}

#[tokio::test]
/// Verifies agent-response events still trigger auto review when the
/// handle has already advanced to `Review` but the paired
/// `SessionUpdated` event has not been reduced yet.
async fn apply_app_events_agent_response_starts_auto_review_from_synced_handle_status() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
    let expected_hash = diff_content_hash(diff_text);
    persist_timeline_test_session(&app, session_id, Status::InProgress).await;

    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-auto-review-sync"),
        ));
    app.sessions.sessions_mut()[0].status = Status::InProgress;
    app.sessions.session_handles_mut().insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::InProgress),
    );
    *app.sessions
        .session_handles()
        .get(session_id)
        .expect("expected session handles")
        .status
        .lock()
        .expect("expected handle status lock") = Status::Review;

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: session_id.into(),
        turn_applied_state: test_turn_applied_state(
            Vec::new(),
            Vec::new(),
            None,
            SessionStats::default(),
        ),
    })
    .await;

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == expected_hash
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::AgentReview);
    assert_eq!(
        *app.sessions
            .session_handles()
            .get(session_id)
            .expect("expected session handles")
            .status
            .lock()
            .expect("expected handle status lock"),
        Status::AgentReview
    );
}

#[tokio::test]
/// Verifies auto review still triggers when the render-loop
/// `sync_from_handles()` has already synced the session snapshot to
/// `Review` before the reducer processes the `AgentResponseReceived`
/// event. This is the primary race condition that caused unreliable
/// auto-review triggering.
async fn apply_app_events_agent_response_starts_auto_review_when_snapshot_already_review() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
    let expected_hash = diff_content_hash(diff_text);
    persist_timeline_test_session(&app, session_id, Status::Review).await;

    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-already-review"),
        ));
    // Simulate sync_from_handles() having already updated the snapshot
    // to `Review` in a prior render tick.
    app.sessions.sessions_mut()[0].status = Status::Review;
    app.sessions.session_handles_mut().insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::Review),
    );
    app.mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: None,
    };

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: session_id.into(),
        turn_applied_state: test_turn_applied_state(
            Vec::new(),
            Vec::new(),
            None,
            SessionStats::default(),
        ),
    })
    .await;

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == expected_hash
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::AgentReview);
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref mode_session_id,
            ..
        } if mode_session_id == session_id
    ));
}

#[tokio::test]
/// Verifies one reducer tick preserves the latest turn projection while
/// accumulating token usage from multiple queued completions.
async fn apply_app_events_agent_response_batches_same_session_turns() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let event_sender = app.services.event_sender();
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-batched-turns"),
        ));

    let first_turn = test_turn_applied_state(
        vec![QuestionItem::new("First question?")],
        Vec::new(),
        None,
        SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            input_tokens: 2,
            output_tokens: 3,
        },
    );
    let second_turn = test_turn_applied_state(
        vec![QuestionItem::new("Latest question?")],
        vec!["Capture reducer batching coverage."],
        None,
        SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            input_tokens: 5,
            output_tokens: 8,
        },
    );

    event_sender
        .send(AppEvent::AgentResponseReceived {
            session_id: "session-1".into(),
            turn_applied_state: second_turn,
        })
        .expect("queued event should send");

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: first_turn,
    })
    .await;

    // Assert
    assert_eq!(
        app.sessions.sessions()[0].questions,
        vec![QuestionItem::new("Latest question?")]
    );
    assert_eq!(app.sessions.sessions()[0].stats.input_tokens, 7);
    assert_eq!(app.sessions.sessions()[0].stats.output_tokens, 11);
    assert_eq!(
        app.sessions.sessions()[0]
            .follow_up_tasks
            .iter()
            .map(|task| task.text.clone())
            .collect::<Vec<_>>(),
        vec!["Capture reducer batching coverage.".to_string()]
    );
}

#[tokio::test]
/// Verifies launching an already-linked follow-up task opens its sibling
/// session instead of creating another session.
async fn launch_or_open_selected_follow_up_task_opens_existing_sibling_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut source_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/source-session"));
    source_session.follow_up_tasks = vec![SessionFollowUpTask {
        id: 1,
        launched_session_id: Some("session-2".into()),
        position: 0,
        text: "Open the sibling session.".to_string(),
    }];
    let mut sibling_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/sibling-session"));
    sibling_session.id = "session-2".into();
    sibling_session.title = Some("Sibling session".to_string());
    app.sessions.push_session(source_session);
    app.sessions.push_session(sibling_session);

    // Act
    app.launch_or_open_selected_follow_up_task("session-1")
        .await
        .expect("follow-up task should open the linked sibling session");

    // Assert
    assert_eq!(app.sessions.selected_session_index(), Some(1));
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            ..
        } if session_id == "session-2"
    ));
}

#[tokio::test]
/// Verifies a stale launched-session link is cleared before replacement
/// session creation starts, so a failed launch does not keep retrying the
/// same orphaned sibling id.
async fn launch_or_open_selected_follow_up_task_clears_stale_sibling_link_before_launch() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut source_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/source-session"));
    source_session.follow_up_tasks = vec![SessionFollowUpTask {
        id: 1,
        launched_session_id: Some("missing-session".into()),
        position: 0,
        text: "Open the sibling session.".to_string(),
    }];
    app.sessions.push_session(source_session);

    // Act
    let result = app
        .launch_or_open_selected_follow_up_task("session-1")
        .await;

    // Assert
    assert!(matches!(
        result,
        Err(AppError::Session(crate::app::SessionError::Workflow(message)))
            if message == "Git branch is required to create a session"
    ));
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(
        app.sessions.sessions()[0].follow_up_tasks[0].launched_session_id,
        None
    );
}

#[tokio::test]
/// Verifies a viewed session keeps its review state when its live status
/// transition reaches `Done`.
async fn apply_app_events_session_updated_keeps_done_view_review_state() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-done-view"),
        ));
    app.sessions.session_handles_mut().insert(
        "session-1".into(),
        SessionHandles::new_with_transcript(
            Status::Done,
            crate::test_support::assistant_transcript("Merge finished"),
        ),
    );
    app.mode = AppMode::View {
        session_id: "session-1".into(),
        scroll_offset: Some(9),
    };

    // Act
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: "session-1".into(),
        version: 1,
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(9),
            ..
        }
    ));
}

#[tokio::test]
/// Verifies refresh keeps the active session view when merge cleanup has
/// removed the worktree just before `Done` persists.
async fn apply_app_events_refresh_keeps_viewed_merging_session_without_worktree() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await;
    let project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session(
            "session-1",
            AgentModel::Gemini3FlashPreview.as_str(),
            "main",
            &Status::Merging.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert merging session");

    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path.clone(),
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");
    let session_folder = base_path.join("session-1");
    let mut viewed_session = crate::test_support::session_fixture_with_folder(session_folder);
    viewed_session.status = Status::Merging;
    app.sessions.push_session(viewed_session);
    app.sessions.session_handles_mut().insert(
        "session-1".into(),
        SessionHandles::new_with_transcript(
            Status::Merging,
            crate::test_support::assistant_transcript("Merging"),
        ),
    );
    app.mode = AppMode::View {
        session_id: "session-1".into(),
        scroll_offset: None,
    };

    // Act
    app.apply_app_events(AppEvent::RefreshSessions).await;

    // Assert
    assert!(
        app.sessions
            .sessions()
            .iter()
            .any(|session| session.id == "session-1" && session.status == Status::Merging)
    );
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id, ..
        } if session_id == "session-1"
    ));
}

#[test]
fn discover_home_project_paths_includes_git_repos_and_excludes_session_worktrees() {
    // Arrange
    let home_directory = tempdir().expect("failed to create temp dir");
    let top_level_repo = home_directory.path().join("agentty");
    create_git_repo_marker(top_level_repo.as_path());
    let nested_repo = home_directory.path().join("code").join("service");
    create_git_repo_marker(nested_repo.as_path());
    let session_worktree_root = home_directory.path().join("agentty-worktrees");
    let session_worktree_repo = session_worktree_root.join("a1b2c3d4");
    create_git_repo_marker(session_worktree_repo.as_path());

    // Act
    let discovered_project_paths =
        App::discover_home_project_paths(home_directory.path(), session_worktree_root.as_path());

    // Assert
    assert!(
        discovered_project_paths.contains(&top_level_repo),
        "top-level git repository should be discovered"
    );
    assert!(
        discovered_project_paths.contains(&nested_repo),
        "nested git repository should be discovered"
    );
    assert!(
        !discovered_project_paths.contains(&session_worktree_repo),
        "session worktree repositories must be excluded"
    );
}

#[test]
fn discover_home_project_paths_respects_repository_limit() {
    // Arrange
    let home_directory = tempdir().expect("failed to create temp dir");
    for index in 0..=HOME_PROJECT_SCAN_MAX_RESULTS {
        let repository = home_directory.path().join(format!("repo-{index}"));
        create_git_repo_marker(repository.as_path());
    }

    // Act
    let discovered_project_paths = App::discover_home_project_paths(
        home_directory.path(),
        Path::new("/tmp/non-session-worktree"),
    );

    // Assert
    assert_eq!(
        discovered_project_paths.len(),
        HOME_PROJECT_SCAN_MAX_RESULTS
    );
}

#[test]
fn is_session_worktree_project_path_returns_true_for_agentty_worktree_path() {
    // Arrange
    let session_worktree_root = Path::new("/home/test/.agentty/wt");
    let project_path = "/home/test/.agentty/wt/a1b2c3d4";

    // Act
    let is_session_worktree =
        App::is_session_worktree_project_path(project_path, session_worktree_root);

    // Assert
    assert!(is_session_worktree);
}

#[test]
fn is_session_worktree_project_path_returns_false_for_main_repository_path() {
    // Arrange
    let session_worktree_root = Path::new("/home/test/.agentty/wt");
    let project_path = "/home/test/src/agentty";

    // Act
    let is_session_worktree =
        App::is_session_worktree_project_path(project_path, session_worktree_root);

    // Assert
    assert!(!is_session_worktree);
}

#[test]
fn is_existing_project_path_returns_true_when_fs_client_reports_directory() {
    // Arrange
    let project_path = "/home/test/src/agentty";
    let expected_path = PathBuf::from(project_path);
    let mut fs_client = crate::infra::fs::MockFsClient::new();
    fs_client
        .expect_is_dir()
        .once()
        .withf(move |path| path == &expected_path)
        .return_const(true);

    // Act
    let project_exists = App::is_existing_project_path(&fs_client, project_path);

    // Assert
    assert!(project_exists);
}

#[test]
fn visible_project_rows_excludes_missing_nongit_and_session_worktree_projects() {
    // Arrange
    let existing_project_path = "/home/test/src/agentty".to_string();
    let nongit_project_path = "/home/test/src/notes".to_string();
    let session_worktree_project_path = "/home/test/.agentty/wt/a1b2c3d4".to_string();
    let missing_project_path = "/home/test/src/removed".to_string();
    let session_worktree_root = Path::new("/home/test/.agentty/wt");
    let project_rows = vec![
        project_list_row_fixture(1, existing_project_path.clone()),
        project_list_row_fixture(2, nongit_project_path.clone()),
        project_list_row_fixture(3, session_worktree_project_path),
        project_list_row_fixture(4, missing_project_path.clone()),
    ];
    let mut fs_client = crate::infra::fs::MockFsClient::new();
    let existing_project_path_for_match = PathBuf::from(existing_project_path.clone());
    let existing_git_marker_for_match = existing_project_path_for_match.join(".git");
    let nongit_project_path_for_match = PathBuf::from(nongit_project_path);
    let missing_project_path_for_match = PathBuf::from(missing_project_path);
    fs_client
        .expect_is_dir()
        .once()
        .withf(move |path| path == &existing_project_path_for_match)
        .return_const(true);
    fs_client
        .expect_exists()
        .once()
        .withf(move |path| path == &existing_git_marker_for_match)
        .return_const(true);
    fs_client
        .expect_is_dir()
        .once()
        .withf(move |path| path == &nongit_project_path_for_match)
        .return_const(true);
    fs_client.expect_exists().once().return_const(false);
    fs_client
        .expect_is_dir()
        .once()
        .withf(move |path| path == &missing_project_path_for_match)
        .return_const(false);

    // Act
    let visible_rows = App::visible_project_rows(project_rows, &fs_client, session_worktree_root);

    // Assert
    assert_eq!(visible_rows.len(), 1);
    assert_eq!(visible_rows[0].path, existing_project_path);
}

#[tokio::test]
async fn resolve_startup_active_project_id_falls_back_when_stored_project_path_is_missing() {
    // Arrange
    let current_project_dir = tempdir().expect("failed to create current project dir");
    let current_project_path = current_project_dir.path().to_path_buf();
    let missing_project_path = current_project_path.join("removed-project");
    let database = AppRepositories::in_memory().await;
    let current_project_id = database
        .projects()
        .upsert_project(
            &current_project_path.to_string_lossy(),
            Some("main".to_string()),
        )
        .await
        .expect("failed to insert current project");
    let missing_project_id = database
        .projects()
        .upsert_project(
            &missing_project_path.to_string_lossy(),
            Some("main".to_string()),
        )
        .await
        .expect("failed to insert missing project");
    database
        .settings()
        .set_active_project_id(missing_project_id)
        .await
        .expect("failed to persist active project");
    let missing_project_path = missing_project_path.clone();
    let mut fs_client = crate::infra::fs::MockFsClient::new();
    fs_client
        .expect_is_dir()
        .once()
        .withf(move |path| path == &missing_project_path)
        .return_const(false);

    // Act
    let resolved_project_id =
        App::resolve_startup_active_project_id(&database, &fs_client, current_project_id).await;

    // Assert
    assert_eq!(resolved_project_id, current_project_id);
}

#[tokio::test]
async fn apply_app_events_refresh_projects_reloads_project_active_session_count() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    fs::create_dir_all(base_path.join(".git")).expect("failed to create project git marker");
    let database = AppRepositories::in_memory().await;
    let project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session(
            "session-active",
            "gemini-3-flash-preview",
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert active session");

    let session_folder_name = "session-".chars().take(8).collect::<String>();
    let session_data_dir = base_path.join(session_folder_name).join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session dir");

    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");

    let initial_active_count = app
        .projects
        .render_parts()
        .project_items
        .iter()
        .find(|item| item.project.id == project_id)
        .map_or(0, |item| item.active_session_count);
    assert_eq!(initial_active_count, 1);

    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at("session-active", &Status::Done.to_string(), 0)
        .await
        .expect("failed to update session status");

    // Act
    app.apply_app_events(AppEvent::RefreshProjects).await;

    // Assert
    let updated_active_count = app
        .projects
        .render_parts()
        .project_items
        .iter()
        .find(|item| item.project.id == project_id)
        .map_or(0, |item| item.active_session_count);
    assert_eq!(updated_active_count, 0);
}

#[tokio::test]
/// Verifies project list loads reuse only persisted rows and do not
/// discover repositories implicitly.
async fn load_project_items_uses_persisted_rows_without_home_scan() {
    // Arrange
    let database = AppRepositories::in_memory().await;
    let home_directory = tempdir().expect("failed to create temp dir");
    let discovered_repo = home_directory.path().join("agentty");
    create_git_repo_marker(discovered_repo.as_path());
    let fs_client = RealFsClient;
    let session_worktree_root = home_directory.path().join(".agentty").join(AGENTTY_WT_DIR);

    // Act
    let project_items = App::load_project_items_with_session_worktree_root(
        &database,
        &fs_client,
        session_worktree_root.as_path(),
    )
    .await;

    // Assert
    assert!(project_items.is_empty());
    assert!(
        database
            .projects()
            .load_projects_with_stats()
            .await
            .expect("failed to load projects")
            .is_empty()
    );
}

#[tokio::test]
/// Verifies the startup-only catalog refresh discovers repositories before
/// the first project list load.
async fn refresh_project_catalog_on_startup_discovers_home_directory_repositories() {
    // Arrange
    let database = AppRepositories::in_memory().await;
    let home_directory = tempdir().expect("failed to create temp dir");
    let discovered_repo = home_directory.path().join("agentty");
    create_git_repo_marker(discovered_repo.as_path());
    let fs_client = RealFsClient;
    let mut mock_git_client = ag_git::MockGitClient::new();
    let session_worktree_root = home_directory.path().join(".agentty").join(AGENTTY_WT_DIR);
    mock_git_client
        .expect_detect_git_info()
        .times(1)
        .returning(|_| Box::pin(async { Some("main".to_string()) }));

    // Act
    App::load_projects_from_home_directory(
        &database,
        &mock_git_client,
        &RealProjectDiscoveryClient,
        session_worktree_root.as_path(),
        Some(home_directory.path()),
    )
    .await;

    let project_items = App::load_project_items_with_session_worktree_root(
        &database,
        &fs_client,
        session_worktree_root.as_path(),
    )
    .await;

    // Assert
    assert_eq!(project_items.len(), 1);
    assert_eq!(project_items[0].project.path, discovered_repo);
    assert_eq!(project_items[0].project.git_branch.as_deref(), Some("main"));
}

/// Creates one directory with a `.git` marker for repository discovery
/// tests.
fn create_git_repo_marker(repository_path: &Path) {
    fs::create_dir_all(repository_path.join(".git"))
        .expect("failed to create repository .git marker");
}

/// Builds one lightweight project row fixture for project list tests.
fn project_list_row_fixture(project_id: i64, project_path: String) -> db::ProjectListRow {
    db::ProjectListRow {
        active_session_count: 0,
        created_at: 0,
        display_name: None,
        git_branch: Some("main".to_string()),
        id: project_id,
        input_tokens: 0,
        is_favorite: false,
        last_opened_at: None,
        last_session_updated_at: None,
        output_tokens: 0,
        path: project_path,
        session_count: 0,
        updated_at: 0,
    }
}

/// Applies queued app events until `condition` observes the expected app
/// state, or fails the test after a short timeout.
async fn wait_for_app_condition(app: &mut App, condition: impl Fn(&App) -> bool) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if condition(app) {
                break;
            }

            let app_event = app
                .next_app_event()
                .await
                .expect("background task should emit an app event");
            app.apply_app_events(app_event).await;
        }
    })
    .await
    .expect("timed out waiting for app condition");
}

/// Replaces the app-level git dependencies with one caller-provided mock.
fn install_mock_git_client(app: &mut App, mock_git_client: ag_git::MockGitClient) {
    let mock_git_client: Arc<dyn ag_git::GitClient> = Arc::new(mock_git_client);
    let base_path = app.services.base_path().to_path_buf();
    let db = app.services.db().clone();
    let event_sender = app.services.event_sender();
    let available_agent_kinds = app.services.available_agent_kinds();
    let available_agent_clis =
        crate::domain::agent::AgentCliInfo::from_kinds(&available_agent_kinds);
    let app_server_client_override = app.services.app_server_client_override();
    let fs_client = app.services.fs_client();
    let review_request_client = app.services.review_request_client();

    app.services = AppServices::new_with_agent_clis(
        base_path,
        app.services.clock(),
        event_sender,
        AppServiceDeps {
            app_server_client_override,
            available_agent_kinds,
            clipboard_image_client_override: None,
            fs_client,
            git_client: Arc::clone(&mock_git_client),
            repositories: db,
            review_request_client,
        },
        available_agent_clis,
    );
}

/// Replaces the app-level review-request dependency with one
/// caller-provided mock.
fn install_mock_review_request_client(
    app: &mut App,
    mock_review_request_client: forge::MockReviewRequestClient,
) {
    let review_request_client: Arc<dyn ReviewRequestClient> = Arc::new(mock_review_request_client);
    let base_path = app.services.base_path().to_path_buf();
    let db = app.services.db().clone();
    let event_sender = app.services.event_sender();
    let app_server_client_override = app.services.app_server_client_override();
    let available_agent_kinds = app.services.available_agent_kinds();
    let available_agent_clis =
        crate::domain::agent::AgentCliInfo::from_kinds(&available_agent_kinds);
    let fs_client = app.services.fs_client();
    let git_client = app.services.git_client();

    app.services = AppServices::new_with_agent_clis(
        base_path,
        app.services.clock(),
        event_sender,
        AppServiceDeps {
            app_server_client_override,
            available_agent_kinds,
            clipboard_image_client_override: None,
            fs_client,
            git_client,
            repositories: db,
            review_request_client,
        },
        available_agent_clis,
    );
}

/// Builds one GitHub remote fixture for requested-review state tests.
fn forge_remote() -> forge::ForgeRemote {
    forge::ForgeRemote {
        command_working_directory: None,
        forge_kind: forge::ForgeKind::GitHub,
        host: "github.com".to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
        web_url: "https://github.com/agentty-xyz/agentty".to_string(),
    }
}

/// Builds one requested-review fixture for app-state detail tests.
fn requested_review() -> RequestedReview {
    RequestedReview {
        audience: RequestedReviewAudience::Personal,
        author: "octocat".to_string(),
        body: Some("Review body".to_string()),
        comment_snapshot: None,
        display_id: "#42".to_string(),
        forge_kind: forge::ForgeKind::GitHub,
        repository: "agentty-xyz/agentty".to_string(),
        status_summary: None,
        title: "Add review detail page".to_string(),
        updated_at: Some("2026-04-27T21:30:00Z".to_string()),
        web_url: "https://example.com/42".to_string(),
    }
}

/// Builds one requested-review comment snapshot fixture for app-state
/// detail tests.
fn review_comment_snapshot() -> forge::ReviewCommentSnapshot {
    forge::ReviewCommentSnapshot {
        pr_level_comments: vec![forge::ReviewComment {
            author: "alice".to_string(),
            body: "Looks good.".to_string(),
        }],
        threads: Vec::new(),
    }
}

#[tokio::test]
async fn test_continue_terminal_session_opens_draft_prompt_for_done_session_with_hash() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await;
    let clients = crate::test_support::test_app_clients()
        .with_app_server_client_override(crate::test_support::mock_app_server())
        .with_tmux_client(Arc::new(MockTmuxClient::new()));
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path.clone(),
        Some("main".to_string()),
        database,
        clients,
    )
    .await
    .expect("failed to build test app");
    let project_id = app
        .services
        .db()
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to insert project");
    app.services
        .db()
        .sessions()
        .insert_session("done-source", "gpt-5.5", "release", "Done", project_id)
        .await
        .expect("failed to insert source session row");
    let merged_commit_hash = "704de31d0f4b5a1234567890abcdef1234567890";
    app.services
        .db()
        .sessions()
        .update_session_merged_commit_hash("done-source", Some(merged_commit_hash.to_string()))
        .await
        .expect("failed to persist merged commit hash");
    let mut source_session = crate::test_support::SessionFixtureBuilder::new()
        .id("done-source")
        .status(Status::Done)
        .project_name("project-alpha")
        .title(Some("Done source".to_string()))
        .build();
    source_session.base_branch = "release".to_string();
    app.sessions.push_session(source_session);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .times(1..)
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
        .expect("expected terminal continuation to succeed");

    // Assert
    assert_ne!(continued_session_id, "done-source");
    assert!(matches!(
        app.mode,
        AppMode::Prompt {
            ref input,
            ref session_id,
            ..
        } if session_id.as_str() == continued_session_id
            && input.text().is_empty()
    ));
    let continued_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == continued_session_id)
        .expect("expected created continuation draft");
    assert!(continued_session.is_draft_session());
    assert_eq!(continued_session.base_branch, "release");
    assert_eq!(continued_session.status, Status::Draft);
    assert_eq!(
        continued_session.prompt,
        format!("Use {merged_commit_hash} commit as an initial context for this session")
    );
    assert!(matches!(
        app.selected_session(),
        Some(session) if session.id == continued_session_id
    ));
}

#[tokio::test]
async fn test_continue_terminal_session_falls_back_to_persisted_context_without_hash() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await;
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
        .insert_session("done-source", "gpt-5.5", "main", "Done", project_id)
        .await
        .expect("failed to insert source session row");
    let source_session = crate::test_support::SessionFixtureBuilder::new()
        .id("done-source")
        .status(Status::Done)
        .summary(Some("# Summary\n\nUse the saved context.".to_string()))
        .title(Some("Done source".to_string()))
        .build();
    app.sessions.push_session(source_session);
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .times(1..)
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
         source\nProject: project\nStatus: Done\n\nPrevious session summary:\n# Summary\n\nUse \
         the saved context.\n"
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
        .summary(Some("summary".to_string()))
        .build();
    app.sessions.push_session(source_session);

    // Act
    let result = app.continue_terminal_session("review-source").await;

    // Assert
    assert!(matches!(
        result,
        Err(AppError::Workflow(message))
            if message == "Only `Done` sessions can be continued"
    ));
}

#[tokio::test]
async fn test_continue_terminal_session_rejects_canceled_source_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let source_session = crate::test_support::SessionFixtureBuilder::new()
        .id("canceled-source")
        .status(Status::Canceled)
        .summary(Some("summary".to_string()))
        .build();
    app.sessions.push_session(source_session);

    // Act
    let result = app.continue_terminal_session("canceled-source").await;

    // Assert
    assert!(matches!(
        result,
        Err(AppError::Workflow(message))
            if message == "Only `Done` sessions can be continued"
    ));
}

#[tokio::test]
async fn test_continue_terminal_session_reports_legacy_session_without_project() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let source_session = crate::test_support::SessionFixtureBuilder::new()
        .id("legacy-source")
        .status(Status::Done)
        .summary(Some("summary".to_string()))
        .build();
    app.sessions.push_session(source_session);

    // Act
    let result = app.continue_terminal_session("legacy-source").await;

    // Assert
    assert!(matches!(
        result,
        Err(AppError::Workflow(message))
            if message == "Source session has no project association. Restart Agentty from \
                this project to backfill legacy sessions, then continue the session again."
    ));
}

#[tokio::test]
async fn apply_review_update_stores_success_in_cache() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-review-cache";
    let review_text = "## Review\nLooks good.";
    let mut session = crate::test_support::session_fixture_with_folder(PathBuf::from(
        "/tmp/session-review-cache",
    ));
    session.id = session_id.to_string().into();
    session.status = Status::AgentReview;
    app.sessions.push_session(session);
    app.sessions.session_handles_mut().insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::AgentReview),
    );
    app.review_cache.insert(
        session_id.to_string().into(),
        ReviewCacheEntry::Loading {
            diff_hash: 123,
            turn_id: 0,
        },
    );

    // Act
    app.apply_review_update(
        session_id,
        ReviewUpdate {
            diff_hash: 123,
            result: Ok(review_text.to_string()),
            turn_id: 0,
        },
    );

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Ready { text, diff_hash }) if text == review_text && *diff_hash == 123
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
    assert_eq!(
        *app.sessions
            .session_handles()
            .get(session_id)
            .expect("expected session handles")
            .status
            .lock()
            .expect("expected handle status lock"),
        Status::Review
    );
}

#[tokio::test]
async fn apply_review_update_stores_failure_in_cache() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-review-fail";
    let error_message = "Review assist failed with exit code 1";
    app.review_cache.insert(
        session_id.to_string().into(),
        ReviewCacheEntry::Loading {
            diff_hash: 456,
            turn_id: 0,
        },
    );

    // Act
    app.apply_review_update(
        session_id,
        ReviewUpdate {
            diff_hash: 456,
            result: Err(error_message.to_string()),
            turn_id: 0,
        },
    );

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Failed { error, diff_hash }) if error == error_message && *diff_hash == 456
    ));
}

#[tokio::test]
async fn apply_review_update_ignores_stale_diff_hash() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-review-stale";
    app.review_cache.insert(
        session_id.to_string().into(),
        ReviewCacheEntry::Loading {
            diff_hash: 999,
            turn_id: 0,
        },
    );

    // Act
    app.apply_review_update(
        session_id,
        ReviewUpdate {
            diff_hash: 111,
            result: Ok("stale review".to_string()),
            turn_id: 0,
        },
    );

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == 999
    ));
}

#[tokio::test]
async fn apply_review_update_keeps_non_agent_review_status_unchanged() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-review-progress";
    let mut session = crate::test_support::session_fixture_with_folder(PathBuf::from(
        "/tmp/session-review-progress",
    ));
    session.id = session_id.to_string().into();
    session.status = Status::InProgress;
    app.sessions.push_session(session);
    app.sessions.session_handles_mut().insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::InProgress),
    );
    app.review_cache.insert(
        session_id.to_string().into(),
        ReviewCacheEntry::Loading {
            diff_hash: 222,
            turn_id: 0,
        },
    );

    // Act
    app.apply_review_update(
        session_id,
        ReviewUpdate {
            diff_hash: 222,
            result: Ok("## Review\nBackground review".to_string()),
            turn_id: 0,
        },
    );

    // Assert
    assert_eq!(app.sessions.sessions()[0].status, Status::InProgress);
    assert_eq!(
        *app.sessions
            .session_handles()
            .get(session_id)
            .expect("expected session handles")
            .status
            .lock()
            .expect("expected handle status lock"),
        Status::InProgress
    );
}

#[tokio::test]
async fn auto_start_reviews_clears_cache_on_in_progress_transition() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-cache-clear"),
        ));
    app.sessions.sessions_mut()[0].status = Status::InProgress;
    app.review_cache.insert(
        session_id.to_string().into(),
        ReviewCacheEntry::Ready {
            diff_hash: 789,
            text: "old review".to_string(),
        },
    );
    let session_ids = HashSet::from([session_id.into()]);

    // Act
    app.auto_start_reviews(&session_ids).await;

    // Assert
    assert!(!app.review_cache.contains_key(session_id));
}

#[tokio::test]
async fn auto_start_reviews_skips_stale_review_status_while_prompt_is_active() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-active-prompt"),
        ));
    app.sessions.sessions_mut()[0].status = Status::Review;
    app.sessions
        .set_active_prompt_output(session_id, " › Reply\n\n".to_string());
    app.review_cache.insert(
        session_id.into(),
        ReviewCacheEntry::Ready {
            diff_hash: 789,
            text: "old review".to_string(),
        },
    );
    let session_ids = HashSet::from([session_id.into()]);
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_diff().times(0);
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.auto_start_reviews(&session_ids).await;

    // Assert
    assert!(!app.review_cache.contains_key(session_id));
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
async fn auto_start_reviews_skips_when_diff_hash_unchanged() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-hash-skip"),
        ));
    app.sessions.sessions_mut()[0].status = Status::Review;

    let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
    let hash = diff_content_hash(diff_text);
    app.review_cache.insert(
        session_id.to_string().into(),
        ReviewCacheEntry::Ready {
            diff_hash: hash,
            text: "existing review".to_string(),
        },
    );
    let session_ids = HashSet::from([session_id.into()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.auto_start_reviews(&session_ids).await;

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Ready { text, .. }) if text == "existing review"
    ));
}

#[tokio::test]
/// Verifies that a review already in `Loading` state with matching diff
/// hash is not re-triggered by a subsequent reducer tick.
async fn auto_start_reviews_skips_when_already_loading_with_same_hash() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-loading-skip"),
        ));
    app.sessions.sessions_mut()[0].status = Status::Review;

    let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
    let hash = diff_content_hash(diff_text);
    app.review_cache.insert(
        session_id.to_string().into(),
        ReviewCacheEntry::Loading {
            diff_hash: hash,
            turn_id: 0,
        },
    );
    let session_ids = HashSet::from([session_id.into()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.auto_start_reviews(&session_ids).await;

    // Assert — still Loading, not re-triggered
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == hash
    ));
    // Status remains Review because mark_session_agent_review was not called.
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
async fn auto_start_reviews_skips_when_auto_review_is_suppressed() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-suppressed-skip"),
        ));
    app.sessions.sessions_mut()[0].status = Status::Review;

    let diff_text = "diff --git a/file.rs b/file.rs\n+stopped turn";
    let hash = diff_content_hash(diff_text);
    app.review_cache.insert(
        session_id.to_string().into(),
        ReviewCacheEntry::Suppressed { diff_hash: hash },
    );
    let session_ids = HashSet::from([session_id.into()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.auto_start_reviews(&session_ids).await;

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Suppressed { diff_hash }) if *diff_hash == hash
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
async fn auto_start_reviews_starts_loading_for_review_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    persist_timeline_test_session(&app, session_id, Status::Review).await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-hash-start"),
        ));
    app.sessions.sessions_mut()[0].status = Status::Review;
    app.sessions
        .session_handles_mut()
        .insert(session_id.into(), SessionHandles::new(Status::Review));

    let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
    let expected_hash = diff_content_hash(diff_text);
    let session_ids = HashSet::from([session_id.into()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.auto_start_reviews(&session_ids).await;

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == expected_hash
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::AgentReview);
}

#[tokio::test]
async fn auto_start_reviews_storage_failure_skips_provider_start() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let (database, pool) = AppRepositories::in_memory_with_pool().await;
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-storage-failure"),
        ));
    app.sessions.sessions_mut()[0].status = Status::Review;
    app.sessions
        .session_handles_mut()
        .insert(session_id.into(), SessionHandles::new(Status::Review));
    let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
    let expected_hash = diff_content_hash(diff_text);
    let session_ids = HashSet::from([session_id.into()]);
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
    install_mock_git_client(&mut app, mock_git_client);
    pool.close().await;

    // Act
    app.auto_start_reviews(&session_ids).await;

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Failed { diff_hash, error })
            if *diff_hash == expected_hash && error.contains("Press f to retry")
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
    let transcript = app
        .sessions
        .session_handles()
        .get(session_id)
        .expect("missing session handles")
        .transcript
        .lock()
        .expect("failed to lock transcript");
    assert!(transcript.messages().iter().any(|message| {
        message.kind == SessionMessageKind::FocusedReview
            && message.state == SessionMessageState::Failed
            && message.content.contains("Press f to retry")
    }));
    assert!(!transcript.has_pending_messages());
}

#[tokio::test]
async fn delete_selected_session_clears_review_cache() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-delete-cache"),
        ));
    app.sessions.select_session_index(Some(0));
    let session_id = app.sessions.sessions()[0].id.clone();
    app.review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Ready {
            diff_hash: 42,
            text: "cached review".to_string(),
        },
    );

    // Act
    app.delete_selected_session().await;

    // Assert
    assert!(!app.review_cache.contains_key(session_id.as_str()));
}

/// Builds one test review request summary for background sync tests.
fn test_review_request_summary(
    display_id: &str,
    state: ReviewRequestState,
) -> ReviewRequestSummary {
    ReviewRequestSummary {
        display_id: display_id.to_string(),
        forge_kind: ForgeKind::GitHub,
        source_branch: "wt/session-id".to_string(),
        state,
        status_summary: None,
        target_branch: "main".to_string(),
        title: "feat".to_string(),
        web_url: String::new(),
    }
}

#[tokio::test]
async fn test_apply_review_request_status_update_ignores_background_errors() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.mode = AppMode::List;

    let update = ReviewRequestStatusUpdate {
        generation: 0,
        result: Err("network timeout".to_string()),
        session_id: "session-1".into(),
    };

    // Act
    app.apply_review_request_status_update(update).await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_apply_review_request_status_update_persists_summary() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-1";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            "gemini-3-flash-preview",
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert session");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    let session_data_dir = app
        .services
        .base_path()
        .join(session_folder_name)
        .join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session data dir");
    app.refresh_sessions_now().await;

    let summary = test_review_request_summary("#5", ReviewRequestState::Open);
    let task_result = SyncReviewRequestTaskResult {
        outcome: session::SyncReviewRequestOutcome::Open {
            display_id: "#5".to_string(),
            status_summary: None,
        },
        summary: Some(summary),
    };

    let update = ReviewRequestStatusUpdate {
        generation: 0,
        result: Ok(task_result),
        session_id: session_id.into(),
    };

    // Act
    app.apply_review_request_status_update(update).await;

    // Assert
    assert_eq!(app.sessions.sessions().len(), 1);
    let session = &app.sessions.sessions()[0];
    let review_request = session
        .review_request
        .as_ref()
        .expect("expected linked review request after sync");
    assert_eq!(review_request.summary.display_id, "#5");
}

#[tokio::test]
async fn test_apply_review_request_status_update_closed_cancels_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-closed";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            "gemini-3-flash-preview",
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert session");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    let session_data_dir = app
        .services
        .base_path()
        .join(session_folder_name)
        .join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session data dir");
    app.refresh_sessions_now().await;

    let task_result = SyncReviewRequestTaskResult {
        outcome: session::SyncReviewRequestOutcome::Closed {
            display_id: "#7".to_string(),
        },
        summary: Some(test_review_request_summary(
            "#7",
            ReviewRequestState::Closed,
        )),
    };

    let update = ReviewRequestStatusUpdate {
        generation: 0,
        result: Ok(task_result),
        session_id: session_id.into(),
    };

    // Act
    app.apply_review_request_status_update(update).await;
    app.process_pending_app_events().await;

    // Assert
    let session = app
        .sessions
        .session_or_err(session_id)
        .expect("expected session to remain loaded");
    assert_eq!(session.status, Status::Canceled);
}

#[tokio::test]
async fn test_apply_review_request_status_update_closed_cancels_stacked_child() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-closed";
    let child_session_id = "session-child";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            "gemini-3-flash-preview",
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert parent session");
    app.services
        .db()
        .sessions()
        .insert_stacked_draft_session(
            child_session_id,
            "gemini-3-flash-preview",
            "wt/session",
            &Status::Draft.to_string(),
            session_id,
            project_id,
        )
        .await
        .expect("failed to insert child session");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    let session_data_dir = app
        .services
        .base_path()
        .join(session_folder_name)
        .join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session data dir");
    app.refresh_sessions_now().await;

    let task_result = SyncReviewRequestTaskResult {
        outcome: session::SyncReviewRequestOutcome::Closed {
            display_id: "#7".to_string(),
        },
        summary: Some(test_review_request_summary(
            "#7",
            ReviewRequestState::Closed,
        )),
    };

    let update = ReviewRequestStatusUpdate {
        generation: 0,
        result: Ok(task_result),
        session_id: session_id.into(),
    };

    // Act
    app.apply_review_request_status_update(update).await;
    app.process_pending_app_events().await;

    // Assert
    let parent_session = app
        .sessions
        .session_or_err(session_id)
        .expect("expected parent session to remain loaded");
    let child_session = app
        .sessions
        .session_or_err(child_session_id)
        .expect("expected child session to remain loaded");
    assert_eq!(parent_session.status, Status::Canceled);
    assert_eq!(child_session.status, Status::Canceled);
}

#[tokio::test]
async fn test_apply_review_request_status_update_merged_marks_session_done() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-merged";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            "gemini-3-flash-preview",
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert session");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    let session_data_dir = app
        .services
        .base_path()
        .join(session_folder_name)
        .join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session data dir");
    app.refresh_sessions_now().await;

    let task_result = SyncReviewRequestTaskResult {
        outcome: session::SyncReviewRequestOutcome::Merged {
            display_id: "#9".to_string(),
            session_head_hash: Some("abc1234".to_string()),
        },
        summary: Some(test_review_request_summary(
            "#9",
            ReviewRequestState::Merged,
        )),
    };

    let update = ReviewRequestStatusUpdate {
        generation: 0,
        result: Ok(task_result),
        session_id: session_id.into(),
    };

    // Act
    app.apply_review_request_status_update(update).await;
    app.process_pending_app_events().await;

    // Assert
    let session = app
        .sessions
        .session_or_err(session_id)
        .expect("expected session to remain loaded");
    assert_eq!(session.status, Status::Done);
    let merged_commit_hash = app
        .services
        .db()
        .sessions()
        .load_session_merged_commit_hash(session_id)
        .await
        .expect("failed to load merged commit hash")
        .expect("expected persisted merged commit hash");
    assert_eq!(merged_commit_hash, "abc1234");
}

#[tokio::test]
async fn test_apply_review_request_status_update_merged_restacks_stacked_child() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-merged";
    let child_session_id = "session-child";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            "gemini-3-flash-preview",
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert parent session");
    app.services
        .db()
        .sessions()
        .insert_stacked_draft_session(
            child_session_id,
            "gemini-3-flash-preview",
            "wt/session",
            &Status::Draft.to_string(),
            session_id,
            project_id,
        )
        .await
        .expect("failed to insert child session");
    app.services
        .db()
        .sessions()
        .update_session_prompt(child_session_id, "Ready to start")
        .await
        .expect("failed to stage child prompt");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    let session_data_dir = app
        .services
        .base_path()
        .join(session_folder_name)
        .join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session data dir");
    app.refresh_sessions_now().await;

    let task_result = SyncReviewRequestTaskResult {
        outcome: session::SyncReviewRequestOutcome::Merged {
            display_id: "#9".to_string(),
            session_head_hash: Some("abc1234".to_string()),
        },
        summary: Some(test_review_request_summary(
            "#9",
            ReviewRequestState::Merged,
        )),
    };

    let update = ReviewRequestStatusUpdate {
        generation: 0,
        result: Ok(task_result),
        session_id: session_id.into(),
    };

    // Act
    app.apply_review_request_status_update(update).await;
    app.process_pending_app_events().await;
    app.refresh_sessions_now().await;
    app.sessions
        .load_session_detail_into_state(app.services.db(), child_session_id)
        .await;

    // Assert
    let child_session = app
        .sessions
        .session_or_err(child_session_id)
        .expect("expected child session to remain loaded");
    assert_eq!(child_session.parent_session_id, None);
    assert_eq!(child_session.base_branch, "main");
    assert!(child_session.can_start_staged_session());

    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    let db_child_session = db_sessions
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing persisted child session");
    assert_eq!(db_child_session.parent_session_id, None);
    assert_eq!(db_child_session.base_branch, "main");
}
