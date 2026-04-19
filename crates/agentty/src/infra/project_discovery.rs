//! Project-discovery boundary used by startup catalog refresh flows.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use ignore::WalkBuilder;

/// Maximum directory depth to scan under the user home for git repositories.
pub(crate) const HOME_PROJECT_SCAN_MAX_DEPTH: usize = 5;

/// Maximum number of repositories discovered from one home-directory scan.
pub(crate) const HOME_PROJECT_SCAN_MAX_RESULTS: usize = 200;

/// Boxed async result returned by [`ProjectDiscoveryClient`] methods.
pub(crate) type ProjectDiscoveryFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Typed error returned by project discovery operations.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ProjectDiscoveryError {
    /// The blocking discovery task panicked or was cancelled.
    #[error("Project discovery task failed: {0}")]
    TaskJoin(tokio::task::JoinError),
}

/// Startup boundary for discovering git repositories on disk.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait ProjectDiscoveryClient: Send + Sync {
    /// Returns git repository roots discovered under `home_directory`.
    ///
    /// # Errors
    /// Returns an error when the blocking discovery task fails to complete.
    fn discover_home_project_paths(
        &self,
        home_directory: PathBuf,
        session_worktree_root: PathBuf,
    ) -> ProjectDiscoveryFuture<Result<Vec<PathBuf>, ProjectDiscoveryError>>;
}

/// Production [`ProjectDiscoveryClient`] implementation backed by `ignore`
/// directory walking on a blocking thread.
pub(crate) struct RealProjectDiscoveryClient;

impl ProjectDiscoveryClient for RealProjectDiscoveryClient {
    fn discover_home_project_paths(
        &self,
        home_directory: PathBuf,
        session_worktree_root: PathBuf,
    ) -> ProjectDiscoveryFuture<Result<Vec<PathBuf>, ProjectDiscoveryError>> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                discover_home_project_paths(
                    home_directory.as_path(),
                    session_worktree_root.as_path(),
                )
            })
            .await
            .map_err(ProjectDiscoveryError::TaskJoin)
        })
    }
}

/// Returns git repository roots discovered under `home_directory`.
pub(crate) fn discover_home_project_paths(
    home_directory: &Path,
    session_worktree_root: &Path,
) -> Vec<PathBuf> {
    let mut discovered_project_paths = Vec::new();

    let mut walker_builder = WalkBuilder::new(home_directory);
    walker_builder
        .max_depth(Some(HOME_PROJECT_SCAN_MAX_DEPTH))
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .ignore(true);
    let walker = walker_builder.build();

    for directory_entry in walker.flatten() {
        let Some(file_type) = directory_entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }

        let directory_path = directory_entry.path();
        if directory_path == home_directory {
            continue;
        }
        if is_session_worktree_project_path(directory_path, session_worktree_root) {
            continue;
        }
        if !directory_path.join(".git").exists() {
            continue;
        }

        discovered_project_paths.push(directory_path.to_path_buf());
        if discovered_project_paths.len() >= HOME_PROJECT_SCAN_MAX_RESULTS {
            break;
        }
    }

    discovered_project_paths.sort();
    discovered_project_paths.dedup();

    discovered_project_paths
}

/// Returns whether `project_path` points to an agentty-managed session
/// worktree under `~/.agentty/wt`.
pub(crate) fn is_session_worktree_project_path(
    project_path: &Path,
    session_worktree_root: &Path,
) -> bool {
    project_path.starts_with(session_worktree_root)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

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
            fs::create_dir_all(project_path.join(".git"))
                .expect("failed to create repository marker");
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
}
