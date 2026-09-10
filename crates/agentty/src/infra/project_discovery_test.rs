use std::fs;

use tempfile::tempdir;

use super::{HOME_PROJECT_SCAN_MAX_RESULTS, discover_home_project_paths};

/// Relative directory name used for session git worktrees within the
/// `agentty` home directory.
const AGENTTY_WT_DIR: &str = "wt";

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
        discover_home_project_paths(home_directory, &session_worktree_root);

    // Assert
    assert_eq!(
        discovered_project_paths,
        vec![project_a_path, project_b_path]
    );
}

/// Verifies home catalog discovery stops after the configured repository
/// limit.
#[test]
fn discover_home_project_paths_respects_repository_limit() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let home_directory = temp_dir.path();
    let session_worktree_root = home_directory.join(".agentty").join(AGENTTY_WT_DIR);

    for index in 0..=HOME_PROJECT_SCAN_MAX_RESULTS {
        let project_path = home_directory.join(format!("project-{index:03}"));
        fs::create_dir_all(project_path.join(".git")).expect("failed to create repository marker");
    }

    // Act
    let discovered_project_paths =
        discover_home_project_paths(home_directory, &session_worktree_root);

    // Assert
    assert_eq!(
        discovered_project_paths.len(),
        HOME_PROJECT_SCAN_MAX_RESULTS
    );
}
