use std::path::Path;
use std::sync::Arc;

use ag_git::{GitClient, MockGitClient};
use ag_session::test_support as model_fixture;

use super::AppStartup;
use crate::app::Tab;
use crate::domain::setting::SettingName;
use crate::infra::db::AppRepositories;
use crate::infra::fs::MockFsClient;

#[tokio::test]
async fn startup_keeps_registered_project_when_git_metadata_is_unavailable() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db");
    let project_id = database
        .projects()
        .upsert_project("/saved/repo", None)
        .await
        .expect("saved project");
    database
        .settings()
        .upsert_project_setting(project_id, SettingName::ProjectCatalogVisible, "true")
        .await
        .expect("registered repository");
    let mut fs = MockFsClient::new();
    fs.expect_exists()
        .once()
        .withf(|path| path == Path::new("/saved/repo/.git"))
        .return_const(false);
    let git: Arc<dyn GitClient> = Arc::new(MockGitClient::new());
    // Act
    let context = AppStartup::load_startup_project_context(
        &database,
        &git,
        &fs,
        Path::new("/saved/repo"),
        None,
        project_id,
    )
    .await
    .expect("startup");
    // Assert
    assert_eq!(context.initial_tab, Tab::Projects);
    assert_eq!(context.project_items.len(), 1);
    assert_eq!(context.project_items[0].project.id, project_id);
}

/// Startup honors the launch project even if a different saved project exists.
#[tokio::test]
async fn startup_uses_launch_project_without_probing_saved_paths() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db");
    let current = database
        .projects()
        .upsert_project("/launch/repo", Some("main".to_string()))
        .await
        .expect("launch");
    let stored = database
        .projects()
        .upsert_project("/protected/Documents/repo", Some("old".to_string()))
        .await
        .expect("stored");
    database
        .settings()
        .set_active_project_id(stored)
        .await
        .expect("active");
    let mut git = MockGitClient::new();
    git.expect_current_upstream_reference()
        .once()
        .withf(|path| path == Path::new("/launch/repo"))
        .returning(|_| Box::pin(async { Ok("origin/main".to_string()) }));
    let git: Arc<dyn GitClient> = Arc::new(git);
    let mut fs = MockFsClient::new();
    fs.expect_exists()
        .once()
        .withf(|path| path == Path::new("/launch/repo/.git"))
        .return_const(true);
    // Act
    let context = AppStartup::load_startup_project_context(
        &database,
        &git,
        &fs,
        Path::new("/launch/repo"),
        Some("main".to_string()),
        current,
    )
    .await
    .expect("context");
    // Assert
    assert_eq!(context.active_project_id, current);
    assert_eq!(context.startup_working_dir, Path::new("/launch/repo"));
    assert_eq!(context.project_items.len(), 2);
}

#[tokio::test]
async fn startup_project_context_reports_project_persistence_failures() {
    // Arrange
    for (trigger_sql, expected_message) in [
        (
            "CREATE TRIGGER reject_project BEFORE UPDATE OF git_branch ON project BEGIN SELECT \
             RAISE(ABORT, 'project rejected'); END",
            "Failed to persist active startup project `/launch/repo`",
        ),
        (
            "CREATE TRIGGER reject_project BEFORE UPDATE OF last_opened_at ON project BEGIN \
             SELECT RAISE(ABORT, 'project rejected'); END",
            "Failed to update startup project activity for `/launch/repo`",
        ),
    ] {
        let (database, pool) = AppRepositories::in_memory_with_pool().await.expect("db");
        let project_id = database
            .projects()
            .upsert_project("/launch/repo", None)
            .await
            .expect("launch project");
        sqlx::query(trigger_sql)
            .execute(&pool)
            .await
            .expect("trigger");
        let git: Arc<dyn GitClient> = Arc::new(MockGitClient::new());
        let fs = MockFsClient::new();

        // Act
        let result = AppStartup::load_startup_project_context(
            &database,
            &git,
            &fs,
            Path::new("/launch/repo"),
            None,
            project_id,
        )
        .await;

        // Assert
        let error = result.err().expect("startup should fail").to_string();
        assert!(error.contains(expected_message), "{error}");
    }
}

#[tokio::test]
async fn startup_project_persistence_reports_session_backfill_failure() {
    // Arrange
    let (database, pool) = AppRepositories::in_memory_with_pool().await.expect("db");
    let project_id = database
        .projects()
        .upsert_project("/legacy/repo", None)
        .await
        .expect("legacy project");
    database
        .sessions()
        .insert_session(
            "legacy-session",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "Review",
            project_id,
        )
        .await
        .expect("legacy session");
    sqlx::query("UPDATE session SET project_id = NULL")
        .execute(&pool)
        .await
        .expect("detach legacy session");
    sqlx::query(
        "CREATE TRIGGER reject_backfill BEFORE UPDATE OF project_id ON session BEGIN SELECT \
         RAISE(ABORT, 'backfill rejected'); END",
    )
    .execute(&pool)
    .await
    .expect("trigger");

    // Act
    let result =
        AppStartup::persist_startup_project(&database, Path::new("/launch/repo"), None).await;

    // Assert
    let error = result.expect_err("startup should fail").to_string();
    assert!(
        error.contains("Failed to backfill startup sessions for project `/launch/repo`"),
        "{error}"
    );
}

#[tokio::test]
async fn startup_project_falls_back_to_launch_directory_when_row_is_missing() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db");

    // Act
    let project =
        AppStartup::load_project(&database, 404, Path::new("/launch/repo"), Some("main")).await;

    // Assert
    assert_eq!(project.id, 404);
    assert_eq!(project.path, Path::new("/launch/repo"));
    assert_eq!(project.git_branch.as_deref(), Some("main"));
    assert_eq!(project.display_name, None);
}

/// Catalog rows are database snapshots, including currently inaccessible paths.
#[tokio::test]
async fn project_catalog_preserves_saved_paths_and_excludes_managed_worktrees() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db");
    database
        .projects()
        .upsert_project("/protected/Documents/repo", None)
        .await
        .expect("saved");
    database
        .projects()
        .upsert_project("/managed/wt/session", Some("main".to_string()))
        .await
        .expect("worktree");
    // Act
    let rows = AppStartup::load_project_items_with_session_worktree_root(
        &database,
        Path::new("/managed/wt"),
    )
    .await;
    // Assert
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].project.path, Path::new("/protected/Documents/repo"));
}

#[tokio::test]
async fn startup_tab_uses_saved_choice_or_first_run_projects() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db");
    // Act / Assert
    assert_eq!(
        AppStartup::load_startup_tab(&database, false).await,
        Tab::Projects
    );
    assert_eq!(
        AppStartup::load_startup_tab(&database, true).await,
        Tab::Sessions
    );
}

#[tokio::test]
async fn catalog_visibility_batch_preserves_order_and_legacy_rows() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db");
    let mut expected_ids = Vec::new();
    for index in 0..200 {
        let id = database
            .projects()
            .upsert_project(&format!("/saved/repo-{index:03}"), None)
            .await
            .expect("project");
        if index % 3 == 0 {
            database
                .settings()
                .upsert_project_setting(id, SettingName::ProjectCatalogVisible, "false")
                .await
                .expect("hidden");
        } else {
            expected_ids.push(id);
            if index % 3 == 1 {
                database
                    .settings()
                    .upsert_project_setting(id, SettingName::ProjectCatalogVisible, "true")
                    .await
                    .expect("visible");
            }
        }
    }
    // Act
    let rows = AppStartup::load_project_items_with_session_worktree_root(
        &database,
        Path::new("/managed/wt"),
    )
    .await;
    // Assert
    assert_eq!(
        rows.iter().map(|row| row.project.id).collect::<Vec<_>>(),
        expected_ids
    );
}
