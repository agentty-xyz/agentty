//! Host layout and removal of task worktree Envs.
//!
//! A task worktree is a sibling of its session's worktree under the Agentty
//! worktree base, on its own branch. The session owns it: deleting or
//! canceling the session removes every task worktree it still keeps.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ag_git as git;
use ag_session::TaskWorktreeId;
use tracing::warn;

use crate::app::SessionManager;
use crate::app::session::SessionError;
use crate::infra::db::AppRepositories;
use crate::infra::fs::{FsClient, FsError};

/// Directory and branch of one task worktree on this host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskWorktreeResources {
    /// Branch the worktree checks out.
    pub(crate) branch: String,
    /// Worktree directory under the Agentty worktree base.
    pub(crate) folder: PathBuf,
    /// Durable identity that names the record to delete after removal.
    pub(crate) id: TaskWorktreeId,
}

impl TaskWorktreeResources {
    /// Places one task worktree beside its session's worktree under `base`.
    pub(crate) fn new(base: &Path, id: TaskWorktreeId) -> Self {
        Self {
            branch: id.branch(),
            folder: base.join(id.directory_name()),
            id,
        }
    }
}

impl SessionManager {
    /// Loads a session's task worktrees, so deletion can still remove them
    /// after the session's records cascade away.
    ///
    /// # Errors
    /// Returns an error when the records cannot be read.
    pub(super) async fn load_task_worktrees(
        repositories: &AppRepositories,
        base: &Path,
        session_id: &str,
    ) -> Result<Vec<TaskWorktreeResources>, SessionError> {
        Ok(repositories
            .task_worktrees()
            .list_task_worktrees(session_id)
            .await?
            .into_iter()
            .map(|row| TaskWorktreeResources::new(base, row.id))
            .collect())
    }

    /// Removes the task worktrees a canceled session keeps, deleting each
    /// record only once its directory and branch are gone.
    ///
    /// A worktree whose removal fails keeps its record in the removing
    /// state, so it still counts toward the session's limit and a later
    /// cleanup can find it. `repo_root` lets a branch be deleted even when
    /// its worktree directory was never fully created; without it, the root
    /// is resolved from each worktree. Callers hold the worktree cleanup
    /// lane.
    pub(super) async fn remove_canceled_task_worktrees(
        repositories: &AppRepositories,
        fs_client: Arc<dyn FsClient>,
        git_client: Arc<dyn git::GitClient>,
        worktrees: Vec<TaskWorktreeResources>,
        repo_root: Option<PathBuf>,
        session_id: &str,
    ) {
        for worktree in worktrees {
            if let Err(error) = repositories
                .task_worktrees()
                .begin_task_worktree_removal(&worktree.id)
                .await
            {
                warn!(%session_id, %error, "failed to mark a task worktree for removal");

                continue;
            }
            let id = worktree.id.clone();
            let cleanup_errors = Self::remove_task_worktree_resources(
                fs_client.as_ref(),
                git_client.as_ref(),
                worktree,
                repo_root.clone(),
            )
            .await;
            if !cleanup_errors.is_empty() {
                Self::warn_cleanup_errors(session_id, &cleanup_errors);

                continue;
            }
            if let Err(error) = repositories
                .task_worktrees()
                .delete_task_worktree(&id)
                .await
            {
                warn!(%session_id, %error, "failed to delete a removed task worktree record");
            }
        }
    }

    /// Removes one task worktree's directory, Git record, and branch.
    ///
    /// A directory or branch that is already gone counts as removed, so a
    /// reservation that never got its directory, or a retry after a partial
    /// removal, can still finish. Returns the failures that leave something
    /// behind, including a repository that cannot be resolved, because the
    /// branch could then survive its record. Deletion and cancellation both
    /// remove task worktrees through this routine.
    pub(super) async fn remove_task_worktree_resources(
        fs_client: &dyn FsClient,
        git_client: &dyn git::GitClient,
        worktree: TaskWorktreeResources,
        repo_root: Option<PathBuf>,
    ) -> Vec<String> {
        let mut cleanup_errors = Vec::new();
        let repo_root = match repo_root {
            Some(repo_root) => Some(repo_root),
            None => git_client
                .main_repo_root(worktree.folder.clone())
                .await
                .ok(),
        };

        if let Err(FsError::Io(error)) = fs_client.remove_dir_all(worktree.folder).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            cleanup_errors.push(format!("failed to remove worktree directory: {error}"));
        }

        let Some(repo_root) = repo_root else {
            cleanup_errors.push("failed to resolve the repository of the branch".to_string());

            return cleanup_errors;
        };
        if let Err(error) = git_client.prune_worktrees(repo_root.clone()).await {
            cleanup_errors.push(format!("failed to prune worktree record: {error}"));
        }
        // A failed deletion only matters while the branch still resolves.
        if let Err(error) = git_client
            .delete_branch(repo_root.clone(), worktree.branch.clone())
            .await
            && git_client
                .ref_hash(repo_root, format!("refs/heads/{}", worktree.branch))
                .await
                .is_ok()
        {
            cleanup_errors.push(format!("failed to delete branch: {error}"));
        }

        cleanup_errors
    }
}
