use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::tempdir;

use super::super::{AGENTTY_WT_DIR, App};
use super::support::{create_git_repo_marker, project_list_row_fixture};
use crate::app::{AppError, Tab};
use crate::domain::agent::AgentModel;
use crate::domain::session::{SESSION_DATA_DIR, Status};
use crate::domain::setting::SettingName;
use crate::infra::db::AppRepositories;
use crate::infra::fs::RealFsClient;
use crate::infra::project_discovery::{HOME_PROJECT_SCAN_MAX_RESULTS, RealProjectDiscoveryClient};
use crate::infra::tmux::MockTmuxClient;
use crate::presentation::setting::SettingsAction;

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

#[tokio::test]
/// Verifies the startup-only catalog refresh discovers repositories before
/// the first project list load.
async fn refresh_project_catalog_on_startup_discovers_home_directory_repositories() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
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

#[tokio::test]
async fn test_switch_project_reloads_project_scoped_settings() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let second_project_dir = tempdir().expect("failed to create second temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
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
            AgentModel::Gpt56Sol.as_str(),
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
    let settings_view = app.settings.view();
    let _ = app
        .settings_presentation
        .apply(&settings_view, SettingsAction::Activate);
    assert!(app.settings_presentation.is_selector_dropdown_open());

    // Act
    app.switch_project(second_project_id)
        .await
        .expect("failed to switch project");

    // Assert
    assert_eq!(
        app.settings.default_smart_selection.model(),
        AgentModel::Gpt56Sol
    );
    assert_eq!(app.settings.launch_configuration, "cargo test");
    assert!(!app.settings_presentation.is_selector_dropdown_open());
    assert_eq!(
        app.settings_presentation
            .snapshot(&app.settings.view())
            .selected_row_index,
        Some(0)
    );
}

#[tokio::test]
async fn resolve_startup_active_project_id_falls_back_when_stored_project_path_is_missing() {
    // Arrange
    let current_project_dir = tempdir().expect("failed to create current project dir");
    let current_project_path = current_project_dir.path().to_path_buf();
    let missing_project_path = current_project_path.join("removed-project");
    let database = AppRepositories::in_memory().await.expect("db should open");
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
async fn test_new_returns_error_when_startup_project_upsert_fails() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    sqlx::query!("DROP TABLE project")
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
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    sqlx::query!("DROP TABLE setting")
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
async fn test_continue_terminal_session_reports_legacy_session_without_project() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let source_session = crate::test_support::SessionFixtureBuilder::new()
        .id("legacy-source")
        .status(Status::Done)
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

#[tokio::test]
/// Verifies project list loads reuse only persisted rows and do not
/// discover repositories implicitly.
async fn load_project_items_uses_persisted_rows_without_home_scan() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
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
    assert_eq!(
        project_items,
        [] as [crate::domain::project::ProjectListItem; 0]
    );
    assert!(
        database
            .projects()
            .load_projects_with_stats()
            .await
            .expect("failed to load projects")
            .is_empty()
    );
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
    let database = AppRepositories::in_memory().await.expect("db should open");
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
            "gemini-3.8-flash",
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
            "gemini-3.8-flash",
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
async fn test_new_with_clients_defaults_to_sessions_when_active_project_exists() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let agentty_home = temp_dir.path().join("agentty-home");
    let project_path = temp_dir.path().join("project");
    fs::create_dir_all(&agentty_home).expect("failed to create agentty home");
    fs::create_dir_all(project_path.join(".git")).expect("failed to create project git marker");
    let database = AppRepositories::in_memory().await.expect("db should open");
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
