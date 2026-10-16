use std::fs;
use std::path::Path;
use std::sync::Arc;

use ag_session::test_support as model_fixture;
use tempfile::tempdir;

use super::super::{AGENTTY_WT_DIR, App};
use super::support::{
    create_git_repo_marker, install_mock_git_client, project_list_row_fixture,
    seed_materialized_session,
};
use crate::app::{AppError, Tab};
use crate::domain::agent::AgentModel;
use crate::domain::session::{SESSION_DATA_DIR, Status};
use crate::domain::setting::SettingName;
use crate::infra::db::AppRepositories;
use crate::infra::tmux::MockTmuxClient;
use crate::presentation::setting::SettingsAction;

/// Root-normalized startup must leave assigned subdirectory history accessible.
#[tokio::test]
async fn test_upgrade_can_open_legacy_subdirectory_sessions_and_settings() {
    // Arrange
    let temp = tempdir().expect("fixture");
    let root = temp.path().join("repo");
    let legacy = root.join("legacy-subdir");
    let base = temp.path().join("wt");
    create_git_repo_marker(&root);
    fs::create_dir_all(&legacy).expect("legacy directory");
    let database = AppRepositories::in_memory().await.expect("db");
    let legacy_id = database
        .projects()
        .upsert_project(&legacy.to_string_lossy(), None)
        .await
        .expect("legacy project");
    seed_materialized_session(
        &database,
        &base,
        legacy_id,
        "legacy-session",
        Status::Review,
    )
    .await;
    database
        .settings()
        .upsert_project_setting(legacy_id, SettingName::LaunchConfiguration, "legacy-launch")
        .await
        .expect("legacy setting");
    let mut app = App::new_with_clients(
        base,
        root.clone(),
        None,
        database.clone(),
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("startup");
    assert_ne!(app.active_project_id(), legacy_id);
    assert!(app.sessions.sessions().is_empty());

    // Act
    app.switch_project(legacy_id)
        .await
        .expect("open legacy project");

    // Assert
    assert_eq!(app.active_project_id(), legacy_id);
    assert_eq!(app.working_dir(), legacy);
    assert_eq!(app.settings.launch_configuration, "legacy-launch");
    assert_eq!(app.sessions.sessions()[0].id.as_str(), "legacy-session");
    assert_eq!(
        database
            .projects()
            .get_project(legacy_id)
            .await
            .expect("project")
            .expect("legacy row")
            .path,
        legacy.to_string_lossy()
    );
}

#[tokio::test]
async fn test_switch_project_rejects_missing_and_non_repository_paths() {
    // Arrange
    let temp = tempdir().expect("fixture");
    let root = temp.path().join("repo");
    create_git_repo_marker(&root);
    let ordinary = temp.path().join("ordinary");
    fs::create_dir_all(&ordinary).expect("ordinary directory");
    let database = AppRepositories::in_memory().await.expect("db");
    let mut app = App::new_with_clients(
        temp.path().join("wt"),
        root,
        None,
        database.clone(),
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("startup");
    // Isolate repository discovery from the checkout containing temporary
    // files.
    let mut git_client = ag_git::MockGitClient::new();
    let ordinary_path = ordinary.clone();
    git_client
        .expect_find_git_repo_root()
        .once()
        .withf(move |path| path == &ordinary_path)
        .returning(|_| Box::pin(async { None }));
    install_mock_git_client(&mut app, git_client);
    let active_id = app.active_project_id();
    for path in [ordinary, temp.path().join("missing")] {
        let id = database
            .projects()
            .upsert_project(&path.to_string_lossy(), None)
            .await
            .expect("project");
        // Act
        let result = app.switch_project(id).await;
        // Assert
        assert!(result.is_err());
        assert_eq!(app.active_project_id(), active_id);
    }
}

#[tokio::test]
async fn test_switch_project_reloads_project_scoped_settings() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let second_project_dir = tempdir().expect("failed to create second temp dir");
    let base_path = base_dir.path().to_path_buf();
    create_git_repo_marker(base_dir.path());
    create_git_repo_marker(second_project_dir.path());
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
            model_fixture::CODEX_MODEL.as_str(),
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
        model_fixture::CODEX_MODEL
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
    let session_worktree_root = home_directory.path().join(".agentty").join(AGENTTY_WT_DIR);

    // Act
    let project_items = App::load_project_items_with_session_worktree_root(
        &database,
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
fn visible_project_rows_keeps_saved_history_without_filesystem_access() {
    // Arrange
    let root = Path::new("/home/test/.agentty/wt");
    let rows = vec![
        project_list_row_fixture(1, "/saved/project".to_string()),
        project_list_row_fixture(2, "/missing/project".to_string()),
        project_list_row_fixture(3, "/home/test/.agentty/wt/session".to_string()),
    ];
    // Act
    let visible = App::visible_project_rows(rows, root);
    // Assert
    assert_eq!(
        visible.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![1, 2]
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
        project_items
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
