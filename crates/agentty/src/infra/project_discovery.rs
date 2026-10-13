//! Project-discovery boundary used by explicit, user-authorized scans.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ignore::WalkBuilder;

/// Maximum directory depth to scan under the selected root for git
/// repositories.
pub(crate) const PROJECT_SCAN_MAX_DEPTH: usize = 5;

/// Maximum number of repositories discovered from one selected-folder scan.
pub(crate) const PROJECT_SCAN_MAX_RESULTS: usize = 200;

/// Boxed async result returned by [`ProjectDiscoveryClient`] methods.
pub(crate) type ProjectDiscoveryFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Typed error returned by project discovery operations.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ProjectDiscoveryError {
    /// The blocking discovery task panicked or was cancelled.
    #[error("Project discovery task failed: {0}")]
    TaskJoin(tokio::task::JoinError),
}

/// User-authorized boundary for discovering git repositories on disk.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait ProjectDiscoveryClient: Send + Sync {
    /// Returns git repository roots discovered under `root`.
    ///
    /// # Errors
    /// Returns an error when the blocking discovery task fails to complete.
    fn discover_project_paths(
        &self,
        root: PathBuf,
        session_worktree_root: PathBuf,
        cancelled: Arc<AtomicBool>,
    ) -> ProjectDiscoveryFuture<Result<Vec<PathBuf>, ProjectDiscoveryError>>;
}

/// Production [`ProjectDiscoveryClient`] implementation backed by `ignore`
/// directory walking on a blocking thread.
pub(crate) struct RealProjectDiscoveryClient;

impl RealProjectDiscoveryClient {
    /// Reads the home path without enumerating or validating directories.
    pub(crate) fn home_directory() -> Option<PathBuf> {
        std::env::home_dir()
    }
}

impl ProjectDiscoveryClient for RealProjectDiscoveryClient {
    fn discover_project_paths(
        &self,
        root: PathBuf,
        session_worktree_root: PathBuf,
        cancelled: Arc<AtomicBool>,
    ) -> ProjectDiscoveryFuture<Result<Vec<PathBuf>, ProjectDiscoveryError>> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                discover_project_paths(root.as_path(), session_worktree_root.as_path(), &cancelled)
            })
            .await
            .map_err(ProjectDiscoveryError::TaskJoin)
        })
    }
}

/// Returns git repository roots discovered under `root`.
pub(crate) fn discover_project_paths(
    root: &Path,
    session_worktree_root: &Path,
    cancelled: &AtomicBool,
) -> Vec<PathBuf> {
    let mut discovered_project_paths = Vec::new();

    let mut walker_builder = WalkBuilder::new(root);
    let excluded_root = session_worktree_root.to_path_buf();
    walker_builder
        .max_depth(Some(PROJECT_SCAN_MAX_DEPTH))
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .ignore(true)
        .follow_links(false)
        .filter_entry(move |entry| !entry.path().starts_with(&excluded_root));
    let walker = walker_builder.build();

    for directory_entry in walker.flatten() {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let Some(file_type) = directory_entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }

        let directory_path = directory_entry.path();
        if is_session_worktree_project_path(directory_path, session_worktree_root) {
            continue;
        }
        if !directory_path.join(".git").exists() {
            continue;
        }

        discovered_project_paths.push(directory_path.to_path_buf());
        if discovered_project_paths.len() >= PROJECT_SCAN_MAX_RESULTS {
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
#[path = "project_discovery_test.rs"]
mod tests;
