use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tempfile::tempdir;

use super::{
    PROJECT_SCAN_MAX_RESULTS, ProjectDiscoveryClient, RealProjectDiscoveryClient,
    discover_project_paths,
};

/// Relative directory name used for session git worktrees within the
/// `agentty` home directory.
const AGENTTY_WT_DIR: &str = "wt";

#[tokio::test]
async fn real_client_completes_selected_folder_scans_and_honors_cancellation() {
    // Arrange
    let root = tempdir().expect("fixture");
    let repo = root.path().join("repo");
    let managed = root.path().join("managed");
    fs::create_dir_all(repo.join(".git")).expect("repository");
    fs::create_dir_all(managed.join("session/.git")).expect("managed worktree");
    fs::write(root.path().join("file"), "ordinary file").expect("file");
    let client = RealProjectDiscoveryClient;

    // Act
    let paths = client
        .discover_project_paths(
            root.path().to_owned(),
            managed.clone(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("scan completes");
    let cancelled = client
        .discover_project_paths(
            root.path().to_owned(),
            managed,
            Arc::new(AtomicBool::new(true)),
        )
        .await
        .expect("cancelled scan completes");

    // Assert
    assert_eq!(paths, vec![repo]);
    assert_eq!(cancelled, Vec::<std::path::PathBuf>::new());
}

/// Verifies home catalog discovery finds repository roots while excluding
/// agentty session worktrees.
#[test]
fn discover_project_paths_includes_git_repos_and_excludes_session_worktrees() {
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
    let discovered_project_paths = discover_project_paths(
        home_directory,
        &session_worktree_root,
        &AtomicBool::new(false),
    );

    // Assert
    assert_eq!(
        discovered_project_paths,
        vec![project_a_path, project_b_path]
    );
}

/// Verifies home catalog discovery stops after the configured repository
/// limit.
#[test]
fn discover_project_paths_respects_repository_limit() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let home_directory = temp_dir.path();
    let session_worktree_root = home_directory.join(".agentty").join(AGENTTY_WT_DIR);

    for index in 0..=PROJECT_SCAN_MAX_RESULTS {
        let project_path = home_directory.join(format!("project-{index:03}"));
        fs::create_dir_all(project_path.join(".git")).expect("failed to create repository marker");
    }

    // Act
    let discovered_project_paths = discover_project_paths(
        home_directory,
        &session_worktree_root,
        &AtomicBool::new(false),
    );

    // Assert
    assert_eq!(discovered_project_paths.len(), PROJECT_SCAN_MAX_RESULTS);
}

#[test]
fn selected_folder_scan_does_not_follow_symlinks_or_parent_ignore_rules() {
    // Arrange
    let root = tempdir().expect("root");
    let selected = root.path().join("selected");
    let local = selected.join("local");
    let outside = root.path().join("outside");
    fs::create_dir_all(local.join(".git")).expect("local");
    fs::create_dir_all(outside.join("repo/.git")).expect("outside");
    fs::write(root.path().join(".ignore"), "local\n").expect("parent ignore");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, selected.join("linked")).expect("link");
    // Act
    let paths = discover_project_paths(
        &selected,
        &root.path().join("managed"),
        &AtomicBool::new(false),
    );
    // Assert
    assert_eq!(paths, vec![local]);
}

#[test]
fn selected_folder_scan_includes_root_repo_and_honors_depth_hidden_and_cancellation() {
    // Arrange
    let root = tempdir().expect("root");
    fs::create_dir_all(root.path().join(".git")).expect("root repo");
    fs::create_dir_all(root.path().join("a/b/c/d/e/f/deep/.git")).expect("deep");
    fs::create_dir_all(root.path().join(".hidden/repo/.git")).expect("hidden");
    let managed = root.path().join("managed");
    // Act
    let paths = discover_project_paths(root.path(), &managed, &AtomicBool::new(false));
    let cancelled = discover_project_paths(root.path(), &managed, &AtomicBool::new(true));
    // Assert
    assert_eq!(paths, vec![root.path().to_owned()]);
    assert_eq!(cancelled, Vec::<std::path::PathBuf>::new());
}
