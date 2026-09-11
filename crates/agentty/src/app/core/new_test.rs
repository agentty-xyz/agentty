use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ag_git::GitClient;
use tempfile::tempdir;
use tokio::process::Command;

use super::super::state::{App, AppClients};
use super::{E2E_DISPLAY_VERSION, current_version_display_text};
use crate::app::AppError;
use crate::app::startup::AppStartup;
use crate::domain::session::Status;
use crate::infra::db;
use crate::infra::db::{AppRepositories, Database};
use crate::infra::fs::FsClient;
use crate::infra::project_discovery::ProjectDiscoveryClient;

const PUBLIC_CONSTRUCTOR_COVERAGE_ENV: &str = "AGENTTY_PUBLIC_CONSTRUCTOR_COVERAGE";

#[test]
fn current_version_display_text_pins_only_explicit_feature_runs() {
    // Arrange / Act
    let pinned_single_digit =
        current_version_display_text(Some(std::ffi::OsStr::new("1")), "0.15.9");
    let pinned_double_digit =
        current_version_display_text(Some(std::ffi::OsStr::new("1")), "0.15.10");
    let unpinned = current_version_display_text(None, "0.15.10");
    let invalid = current_version_display_text(Some(std::ffi::OsStr::new("true")), "0.15.10");

    // Assert
    assert_eq!(pinned_single_digit, E2E_DISPLAY_VERSION);
    assert_eq!(pinned_double_digit, pinned_single_digit);
    assert_eq!(unpinned, "v0.15.10");
    assert_eq!(invalid, unpinned);
}

#[tokio::test]
async fn startup_preserves_unregistered_replay_history() {
    // Arrange
    let root = tempdir().expect("worktrees");
    let archive = root.path().join("session/.agentty-replay-orphan");
    fs::create_dir_all(&archive).expect("archive");
    fs::write(archive.join(".gitignore"), "*\n").expect("marker");
    fs::write(archive.join("history.md"), "private history").expect("history");
    let db = AppRepositories::in_memory().await.expect("database");

    // Act
    let result = App::new_with_clients(
        root.path().to_owned(),
        root.path().to_owned(),
        None,
        db,
        crate::test_support::test_app_clients(),
    )
    .await;

    // Assert
    assert!(result.is_ok());
    assert_eq!(
        fs::read_to_string(archive.join("history.md")).expect("preserved history"),
        "private history"
    );
}

#[tokio::test]
async fn startup_reports_archive_cleanup_failures() {
    // Arrange
    let root = tempdir().expect("worktrees");
    let db = AppRepositories::in_memory().await.expect("database");
    let mut fs_client = crate::infra::fs::MockFsClient::new();
    fs_client
        .expect_cleanup_agent_artifacts()
        .once()
        .returning(|_| {
            Box::pin(async { Err(std::io::Error::other("archive cleanup failed").into()) })
        });
    let mut clients = crate::test_support::test_app_clients();
    clients.fs_client = Arc::new(fs_client);

    // Act
    let result = App::new_with_clients(
        root.path().to_owned(),
        root.path().to_owned(),
        None,
        db,
        clients,
    )
    .await;

    // Assert
    let error = result.err().expect("startup must stop");
    assert!(error.to_string().contains("archive cleanup failed"));
    assert!(
        error
            .to_string()
            .contains("Startup recovery did not complete")
    );
}

#[tokio::test]
/// Verifies the public constructor starts with its production client
/// bundle in an isolated environment.
async fn test_new_uses_production_client_bundle() {
    if std::env::var_os(PUBLIC_CONSTRUCTOR_COVERAGE_ENV).is_some() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory database");

        // Act
        let app = App::new(false, base_path.clone(), base_path, None, database)
            .await
            .expect("public constructor should build app");

        // Assert
        assert!(app.selected_session().is_none());

        return;
    }

    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let stub_bin = base_dir.path().join("stub-bin");
    fs::create_dir_all(&stub_bin).expect("failed to create stub bin directory");
    let codex_stub = stub_bin.join("codex");
    fs::write(&codex_stub, "#!/bin/sh\nexit 0\n").expect("failed to write codex stub");
    fs::set_permissions(&codex_stub, fs::Permissions::from_mode(0o750))
        .expect("failed to mark codex stub executable");
    let test_binary = std::env::current_exe().expect("failed to locate test binary");
    let child_path = format!("{}:/usr/bin:/bin", stub_bin.display());
    let child_coverage_profile = std::env::var_os("LLVM_PROFILE_FILE").map(|profile| {
        profile
            .to_string_lossy()
            .replace(".profraw", "-child.profraw")
    });

    // Act
    let mut child = Command::new(test_binary);
    child
        .arg("--exact")
        .arg("app::core::new::tests::test_new_uses_production_client_bundle")
        .arg("--nocapture")
        .env(PUBLIC_CONSTRUCTOR_COVERAGE_ENV, "1")
        .env("HOME", base_dir.path())
        .env("PATH", child_path);
    if let Some(profile) = child_coverage_profile {
        child.env("LLVM_PROFILE_FILE", profile);
    }
    let child_status = child
        .status()
        .await
        .expect("failed to run isolated constructor test");

    // Assert
    assert!(child_status.success());
}

#[tokio::test]
/// Verifies incomplete recovery prevents startup from admitting sessions.
async fn test_new_with_clients_returns_actionable_error_when_recovery_fails() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    sqlx::query("DROP TABLE session_operation")
        .execute(&pool)
        .await
        .expect("failed to remove session operation table");

    // Act
    let error = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .err();

    // Assert
    assert!(
        error.is_some(),
        "incomplete recovery should prevent app startup"
    );
    if let Some(error) = error {
        assert!(matches!(error, AppError::Workflow(_)));
        assert!(
            error
                .to_string()
                .contains("Startup recovery did not complete")
        );
        assert!(error.to_string().contains("restart Agentty"));
    }
}

#[tokio::test]
/// Verifies a subsequent startup admits sessions after recovery is
/// retried following a transient operation-update failure.
async fn test_new_with_clients_retries_recovery_after_operation_update_failure() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session(
            "sess1",
            "gemini-3.8-flash",
            "main",
            &Status::InProgress.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert session");
    database
        .operations()
        .insert_session_operation("op-1", "sess1", "reply")
        .await
        .expect("failed to insert session operation");
    sqlx::query(
        "CREATE TRIGGER fail_startup_recovery BEFORE UPDATE OF status ON session_operation BEGIN \
         SELECT RAISE(FAIL, 'operation update failed'); END",
    )
    .execute(&pool)
    .await
    .expect("failed to create recovery trigger");

    // Act
    let failed_startup = App::new_with_clients(
        base_path.clone(),
        base_path.clone(),
        None,
        database.clone(),
        crate::test_support::test_app_clients(),
    )
    .await;
    sqlx::query("DROP TRIGGER fail_startup_recovery")
        .execute(&pool)
        .await
        .expect("failed to remove recovery trigger");
    let retried_startup = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await;

    // Assert
    assert!(matches!(failed_startup, Err(AppError::Workflow(_))));
    assert!(retried_startup.is_ok());
}

impl App {
    /// Builds app state from persisted data with explicit external clients.
    ///
    /// Auto-update is disabled by default; use [`App::new`] with an explicit
    /// `auto_update` flag for production startup.
    ///
    /// # Errors
    /// Returns an error if startup project metadata cannot be persisted,
    /// required startup state cannot be loaded from the database, or restart
    /// recovery cannot complete.
    pub(crate) async fn new_with_clients(
        base_path: PathBuf,
        working_dir: PathBuf,
        git_branch: Option<String>,
        repositories: impl Into<AppRepositories>,
        clients: AppClients,
    ) -> Result<Self, AppError> {
        Self::new_with_options(
            false,
            base_path,
            working_dir,
            git_branch,
            format!("v{}", env!("CARGO_PKG_VERSION")),
            repositories.into(),
            clients,
        )
        .await
    }

    /// Resolves startup active project id from settings, falling back to the
    /// current working directory when the stored project row is stale.
    pub(in crate::app::core) async fn resolve_startup_active_project_id(
        db: &AppRepositories,
        fs_client: &dyn FsClient,
        current_project_id: i64,
    ) -> i64 {
        AppStartup::resolve_startup_active_project_id(db, fs_client, current_project_id).await
    }

    /// Loads project list entries with one caller-provided session worktree
    /// root for filtering.
    pub(in crate::app::core) async fn load_project_items_with_session_worktree_root(
        db: &AppRepositories,
        fs_client: &dyn FsClient,
        session_worktree_root: &Path,
    ) -> Vec<crate::domain::project::ProjectListItem> {
        AppStartup::load_project_items_with_session_worktree_root(
            db,
            fs_client,
            session_worktree_root,
        )
        .await
    }

    /// Refreshes the persisted project catalog from the user's home directory
    /// during startup before the first project list render.
    pub(in crate::app::core) async fn load_projects_from_home_directory(
        db: &AppRepositories,
        git_client: &dyn GitClient,
        project_discovery_client: &dyn ProjectDiscoveryClient,
        session_worktree_root: &Path,
        home_directory: Option<&Path>,
    ) {
        AppStartup::load_projects_from_home_directory(
            db,
            git_client,
            project_discovery_client,
            session_worktree_root,
            home_directory,
        )
        .await;
    }

    /// Returns git repository roots discovered under the user home directory.
    ///
    /// A repository root is identified by a direct `.git` marker inside the
    /// directory and discovery stops after `HOME_PROJECT_SCAN_MAX_RESULTS`.
    pub(in crate::app::core) fn discover_home_project_paths(
        home_directory: &Path,
        session_worktree_root: &Path,
    ) -> Vec<PathBuf> {
        AppStartup::discover_home_project_paths(home_directory, session_worktree_root)
    }

    /// Returns whether a persisted project path points to an agentty session
    /// worktree under `~/.agentty/wt`.
    pub(in crate::app::core) fn is_session_worktree_project_path(
        project_path: &str,
        session_worktree_root: &Path,
    ) -> bool {
        AppStartup::is_session_worktree_project_path(project_path, session_worktree_root)
    }

    /// Filters persisted project rows down to git repository entries that
    /// should remain visible in the Projects tab.
    pub(in crate::app::core) fn visible_project_rows(
        project_rows: Vec<db::ProjectListRow>,
        fs_client: &dyn FsClient,
        session_worktree_root: &Path,
    ) -> Vec<db::ProjectListRow> {
        AppStartup::visible_project_rows(project_rows, fs_client, session_worktree_root)
    }

    /// Returns whether one persisted project path still resolves to a
    /// directory on disk.
    pub(in crate::app::core) fn is_existing_project_path(
        fs_client: &dyn FsClient,
        project_path: &str,
    ) -> bool {
        AppStartup::is_existing_project_path(fs_client, project_path)
    }
}
