use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use super::super::core::AGENTTY_WT_DIR;
use super::AppStartup;
use crate::infra::db::AppRepositories;
use crate::infra::fs::MockFsClient;

/// Builds one project-list row fixture for startup filtering tests.
fn project_list_row_fixture(
    project_id: i64,
    project_path: &Path,
) -> crate::infra::db::ProjectListRow {
    crate::infra::db::ProjectListRow {
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
        path: project_path.to_string_lossy().to_string(),
        session_count: 0,
        updated_at: 0,
    }
}

/// Builds one mock filesystem client that reports directories from the
/// provided set.
fn mock_fs_client_with_directories(existing_directories: HashSet<PathBuf>) -> MockFsClient {
    let mut fs_client = MockFsClient::new();
    fs_client
        .expect_is_dir()
        .returning(move |path| existing_directories.contains(&path));

    fs_client
}

/// Builds one mock filesystem client that reports existing directories and
/// direct `.git` metadata markers from separate sets.
fn mock_fs_client_with_directories_and_git_markers(
    existing_directories: HashSet<PathBuf>,
    git_marker_paths: HashSet<PathBuf>,
) -> MockFsClient {
    let mut fs_client = MockFsClient::new();
    fs_client
        .expect_is_dir()
        .returning(move |path| existing_directories.contains(&path));
    fs_client
        .expect_exists()
        .returning(move |path| git_marker_paths.contains(&path));

    fs_client
}

/// Verifies startup project resolution prefers the persisted active
/// project when its directory still exists.
#[tokio::test]
async fn resolve_startup_active_project_id_prefers_existing_stored_project() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let current_project_path = PathBuf::from("/workspace/current");
    let stored_project_path = PathBuf::from("/workspace/stored");
    let current_project_id = database
        .projects()
        .upsert_project(
            &current_project_path.to_string_lossy(),
            Some("main".to_string()),
        )
        .await
        .expect("failed to persist current project");
    let stored_project_id = database
        .projects()
        .upsert_project(
            &stored_project_path.to_string_lossy(),
            Some("main".to_string()),
        )
        .await
        .expect("failed to persist stored project");
    database
        .settings()
        .set_active_project_id(stored_project_id)
        .await
        .expect("failed to persist active project id");
    let fs_client =
        mock_fs_client_with_directories(HashSet::from([current_project_path, stored_project_path]));

    // Act
    let resolved_project_id =
        AppStartup::resolve_startup_active_project_id(&database, &fs_client, current_project_id)
            .await;

    // Assert
    assert_eq!(resolved_project_id, stored_project_id);
}

/// Verifies startup project resolution falls back to the current project
/// when the persisted active path is stale.
#[tokio::test]
async fn resolve_startup_active_project_id_falls_back_for_missing_stored_project() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let current_project_path = PathBuf::from("/workspace/current");
    let missing_project_path = PathBuf::from("/workspace/missing");
    let current_project_id = database
        .projects()
        .upsert_project(
            &current_project_path.to_string_lossy(),
            Some("main".to_string()),
        )
        .await
        .expect("failed to persist current project");
    let missing_project_id = database
        .projects()
        .upsert_project(
            &missing_project_path.to_string_lossy(),
            Some("main".to_string()),
        )
        .await
        .expect("failed to persist missing project");
    database
        .settings()
        .set_active_project_id(missing_project_id)
        .await
        .expect("failed to persist active project id");
    let fs_client = mock_fs_client_with_directories(HashSet::from([current_project_path]));

    // Act
    let resolved_project_id =
        AppStartup::resolve_startup_active_project_id(&database, &fs_client, current_project_id)
            .await;

    // Assert
    assert_eq!(resolved_project_id, current_project_id);
}

/// Verifies visible project filtering removes missing directories,
/// non-git folders, and agentty-managed worktree paths.
#[test]
fn visible_project_rows_excludes_missing_nongit_and_worktree_projects() {
    // Arrange
    let visible_project_path = PathBuf::from("/workspace/visible");
    let nongit_project_path = PathBuf::from("/workspace/nongit");
    let missing_project_path = PathBuf::from("/workspace/missing");
    let session_worktree_root = Path::new("/workspace/.agentty/wt");
    let worktree_project_path = session_worktree_root.join("session-a");
    let project_rows = vec![
        project_list_row_fixture(1, &visible_project_path),
        project_list_row_fixture(2, &nongit_project_path),
        project_list_row_fixture(3, &missing_project_path),
        project_list_row_fixture(4, &worktree_project_path),
    ];
    let fs_client = mock_fs_client_with_directories_and_git_markers(
        HashSet::from([
            visible_project_path.clone(),
            nongit_project_path,
            worktree_project_path,
        ]),
        HashSet::from([visible_project_path.join(".git")]),
    );

    // Act
    let visible_rows =
        AppStartup::visible_project_rows(project_rows, &fs_client, session_worktree_root);

    // Assert
    assert_eq!(visible_rows.len(), 1);
    assert_eq!(visible_rows[0].path, visible_project_path.to_string_lossy());
}

/// Verifies project-item loading uses database rows and filters non-git
/// folders plus agentty-managed worktree paths before building UI items.
#[tokio::test]
async fn load_project_items_with_session_worktree_root_filters_database_rows() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let visible_project_path = PathBuf::from("/workspace/visible");
    let nongit_project_path = PathBuf::from("/workspace/nongit");
    let session_worktree_root = Path::new("/workspace/.agentty/wt");
    let worktree_project_path = session_worktree_root.join("session-a");
    database
        .projects()
        .upsert_project(
            &visible_project_path.to_string_lossy(),
            Some("main".to_string()),
        )
        .await
        .expect("failed to persist visible project");
    database
        .projects()
        .upsert_project(&nongit_project_path.to_string_lossy(), None)
        .await
        .expect("failed to persist nongit project");
    database
        .projects()
        .upsert_project(
            &worktree_project_path.to_string_lossy(),
            Some("main".to_string()),
        )
        .await
        .expect("failed to persist worktree project");
    let fs_client = mock_fs_client_with_directories_and_git_markers(
        HashSet::from([
            visible_project_path.clone(),
            nongit_project_path,
            worktree_project_path,
        ]),
        HashSet::from([visible_project_path.join(".git")]),
    );

    // Act
    let project_items = AppStartup::load_project_items_with_session_worktree_root(
        &database,
        &fs_client,
        session_worktree_root,
    )
    .await;

    // Assert
    assert_eq!(project_items.len(), 1);
    assert_eq!(project_items[0].project.path, visible_project_path);
}

/// Verifies home catalog discovery finds repository roots while excluding
/// agentty session worktrees.
#[test]
fn discover_home_project_paths_includes_git_repos_and_excludes_session_worktrees() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let home_directory = temp_dir.path();
    let project_a_path = home_directory.join("project-a");
    let project_b_path = home_directory.join("project-b");
    let session_worktree_root = home_directory.join(".agentty").join(AGENTTY_WT_DIR);
    let session_project_path = session_worktree_root.join("session-a");
    fs::create_dir_all(project_a_path.join(".git")).expect("failed to create first repo");
    fs::create_dir_all(project_b_path.join(".git")).expect("failed to create second repo");
    fs::create_dir_all(session_project_path.join(".git"))
        .expect("failed to create session worktree repo");

    // Act
    let discovered_project_paths =
        AppStartup::discover_home_project_paths(home_directory, &session_worktree_root);

    // Assert
    assert_eq!(
        discovered_project_paths,
        vec![project_a_path, project_b_path]
    );
}

impl AppStartup {
    /// Returns git repository roots discovered under the user home directory.
    ///
    /// Tests call through the real project-discovery implementation so app
    /// orchestration no longer owns raw filesystem walking logic.
    pub(crate) fn discover_home_project_paths(
        home_directory: &Path,
        session_worktree_root: &Path,
    ) -> Vec<PathBuf> {
        crate::infra::project_discovery::discover_home_project_paths(
            home_directory,
            session_worktree_root,
        )
    }
}
