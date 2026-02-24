//! Active-project context management for the working directory.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Project domain state and git status tracking for the active working
/// directory.
pub struct ProjectManager {
    active_project_id: i64,
    git_branch: Option<String>,
    git_status: Option<(u32, u32)>,
    git_status_cancel: Arc<AtomicBool>,
    working_dir: PathBuf,
}

impl ProjectManager {
    /// Creates a project manager with initial state.
    pub fn new(
        active_project_id: i64,
        git_branch: Option<String>,
        git_status_cancel: Arc<AtomicBool>,
        working_dir: PathBuf,
    ) -> Self {
        Self {
            active_project_id,
            git_branch,
            git_status: None,
            git_status_cancel,
            working_dir,
        }
    }

    /// Returns the active project identifier.
    pub(crate) fn active_project_id(&self) -> i64 {
        self.active_project_id
    }

    /// Returns the git branch of the active project, when available.
    pub(crate) fn git_branch(&self) -> Option<&str> {
        self.git_branch.as_deref()
    }

    /// Returns the latest ahead/behind snapshot.
    pub(crate) fn git_status(&self) -> Option<(u32, u32)> {
        self.git_status
    }

    /// Returns the active project working directory.
    pub(crate) fn working_dir(&self) -> &Path {
        self.working_dir.as_path()
    }

    /// Returns the project name derived from the working directory's folder
    /// name.
    pub(crate) fn project_name(&self) -> String {
        self.working_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string()
    }

    /// Returns the current git-status cancellation token.
    pub(crate) fn git_status_cancel(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.git_status_cancel)
    }

    /// Updates the last known git status.
    pub(crate) fn set_git_status(&mut self, git_status: Option<(u32, u32)>) {
        self.git_status = git_status;
    }

    /// Returns whether a git branch is configured for the active project.
    pub(crate) fn has_git_branch(&self) -> bool {
        self.git_branch.is_some()
    }
}

#[cfg(test)]
impl ProjectManager {
    /// Replaces the active project context values.
    ///
    /// Only available in test builds to set up test-specific state without
    /// exposing mutable context updates to production code.
    pub(crate) fn replace_context(
        &mut self,
        active_project_id: i64,
        git_branch: Option<String>,
        working_dir: PathBuf,
    ) {
        self.active_project_id = active_project_id;
        self.git_branch = git_branch;
        self.working_dir = working_dir;
    }
}
