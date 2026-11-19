//! Startup sweep that removes leftover worktrees of archived sessions.
//!
//! Archiving a session removes its worktree in a background task. A task cut
//! off by shutdown, a crash, or a failure leaves the directory behind, so
//! startup reconciles the worktree base against the archived session rows
//! captured before the app handles any event.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use super::{session_branch, session_folder};
use crate::app::{AppServices, SessionManager, spawn};
use crate::domain::session::Status;
use crate::infra::db::{AppRepositories, SessionAgentModelRow, SessionRow};

/// Archived sessions captured at startup whose leftover directories one
/// background task removes.
///
/// Capture happens before the foreground loop handles any event, so no
/// archive-time cleanup of this process can target a captured session: those
/// cleanups start only on the transition into `Done` or `Canceled`, which
/// captured sessions completed in an earlier run. Removal holds the worktree
/// cleanup lane, so it never overlaps a deletion or another archived-session
/// cleanup either.
pub(crate) struct ArchivedWorktreeSweep {
    /// Archived sessions in capture order.
    candidates: Vec<ArchivedSession>,
    /// Worktree folder names that a non-archived session mapped to at
    /// capture time.
    claimed_folder_names: HashSet<String>,
}

/// One archived session captured for the sweep.
struct ArchivedSession {
    id: String,
    project_id: Option<i64>,
}

impl ArchivedWorktreeSweep {
    /// Captures the archived sessions whose folders the sweep may remove.
    ///
    /// Captures nothing when the rows cannot be loaded; the next startup
    /// tries again.
    pub(crate) async fn capture(db: &AppRepositories) -> Self {
        let mut candidates = Vec::new();
        let mut claimed_folder_names = HashSet::new();
        let rows = match db.sessions().load_sessions().await {
            Ok(rows) => rows,
            Err(error) => {
                warn!(%error, "failed to load sessions for the archived worktree sweep");

                Vec::new()
            }
        };
        for row in rows {
            if !is_archived(&row) {
                claimed_folder_names.extend(folder_name(&row.id).map(str::to_string));
            } else if is_safe_session_id(&row.id) {
                candidates.push(ArchivedSession {
                    id: row.id,
                    project_id: row.project_id,
                });
            }
        }

        Self {
            candidates,
            claimed_folder_names,
        }
    }

    /// Runs the sweep in a tracked background task, so it neither delays
    /// startup nor outlives the shared shutdown grace period. Starts no task
    /// when no archived session was captured.
    pub(crate) fn spawn(self, services: &AppServices) {
        if self.candidates.is_empty() {
            return;
        }
        let sweep_services = services.clone();
        services.track_cleanup_task(spawn::spawn_detached(async move {
            self.run(&sweep_services).await;
        }));
    }

    /// Removes leftover directories one session at a time, logging failures
    /// and continuing with the next session.
    ///
    /// A candidate with neither folder on disk is skipped before the lane is
    /// taken or any row is reloaded, so a healthy startup costs only one
    /// filesystem check per archived session.
    async fn run(self, services: &AppServices) {
        for candidate in &self.candidates {
            if !Self::has_leftover_folder(services, candidate) {
                continue;
            }
            let cleanup_errors = self.sweep_session(services, candidate).await;
            for cleanup_error in cleanup_errors {
                warn!(session_id = %candidate.id, "archived worktree sweep: {cleanup_error}");
            }
        }
    }

    /// Returns whether the worktree folder or the legacy draft folder of one
    /// captured session is still a removable directory.
    ///
    /// This is only a pre-filter; [`Self::sweep_session`] checks each folder
    /// again under the lane because it can disappear in between.
    fn has_leftover_folder(services: &AppServices, candidate: &ArchivedSession) -> bool {
        let base_path = services.base_path();

        is_removable_directory(services, &session_folder(base_path, &candidate.id))
            || is_removable_directory(services, &base_path.join(&candidate.id))
    }

    /// Removes the worktree and legacy draft folder of one captured session
    /// after confirming, under the worktree cleanup lane, that its row still
    /// exists, is still archived, and shares its folder with no other
    /// session.
    async fn sweep_session(
        &self,
        services: &AppServices,
        candidate: &ArchivedSession,
    ) -> Vec<String> {
        let lane = services.worktree_cleanup_lane();
        let _worktree_cleanup_turn = lane.lock().await;
        let sessions = services.db().sessions();
        let active_rows = match (
            sessions.load_session(&candidate.id).await,
            sessions.load_active_session_agent_models().await,
        ) {
            (Ok(Some(row)), Ok(active_rows)) if is_archived(&row) => active_rows,
            (Err(error), _) | (_, Err(error)) => {
                return vec![format!("failed to reload sessions: {error}")];
            }
            _ => {
                debug!(session_id = %candidate.id, "archived worktree sweep: session changed");

                return Vec::new();
            }
        };

        let mut cleanup_errors = Vec::new();
        let fs_client = services.fs_client();
        let base_path = services.base_path();
        let folder = session_folder(base_path, &candidate.id);
        if !self.is_shared_folder(&candidate.id, &active_rows)
            && is_removable_directory(services, &folder)
        {
            let repo_root = Self::repository_root(services, &folder, candidate.project_id).await;
            let remove_git_resources = repo_root.is_some();
            cleanup_errors.extend(
                SessionManager::cleanup_session_worktree_resources(
                    fs_client.clone(),
                    services.git_client(),
                    folder.clone(),
                    session_branch(&candidate.id),
                    repo_root,
                    remove_git_resources,
                )
                .await,
            );
        }

        // Before #1083, staged draft attachments lived in `<base>/<full id>`;
        // only deletion removed that folder, so archived sessions kept it.
        let legacy_draft_folder = base_path.join(&candidate.id);
        if legacy_draft_folder != folder
            && is_removable_directory(services, &legacy_draft_folder)
            && let Err(error) = fs_client.remove_dir_all(legacy_draft_folder).await
        {
            cleanup_errors.push(format!("failed to remove legacy draft directory: {error}"));
        }

        cleanup_errors
    }

    /// Returns whether a non-archived session maps to the same worktree
    /// folder as `session_id`, either at capture time or among the
    /// `active_rows` reloaded under the worktree cleanup lane.
    fn is_shared_folder(&self, session_id: &str, active_rows: &[SessionAgentModelRow]) -> bool {
        let name = folder_name(session_id);

        name.is_some_and(|name| self.claimed_folder_names.contains(name))
            || active_rows
                .iter()
                .any(|row| row.id != session_id && folder_name(&row.id) == name)
    }

    /// Resolves the repository that owns the worktree record.
    ///
    /// Asks Git only when the folder still has its linked-worktree `.git`
    /// file or the session's project checkout still has Git metadata, so a
    /// partly deleted worktree never resolves to an unrelated repository
    /// above the worktree base.
    async fn repository_root(
        services: &AppServices,
        folder: &Path,
        project_id: Option<i64>,
    ) -> Option<PathBuf> {
        let fs_client = services.fs_client();
        let git_client = services.git_client();
        if fs_client.is_file(folder.join(".git"))
            && let Ok(repo_root) = git_client.main_repo_root(folder.to_path_buf()).await
        {
            return Some(repo_root);
        }
        let project_id = project_id?;
        let project = services
            .db()
            .projects()
            .get_project(project_id)
            .await
            .ok()
            .flatten();
        let project_path = PathBuf::from(project?.path);
        if !fs_client.exists(project_path.join(".git")) {
            return None;
        }

        git_client.main_repo_root(project_path).await.ok()
    }
}

/// Returns whether a persisted row is archived as `Done` or `Canceled`.
fn is_archived(row: &SessionRow) -> bool {
    matches!(
        row.status.parse::<Status>(),
        Ok(Status::Done | Status::Canceled)
    )
}

/// Returns whether `session_id` can only name a direct child of the worktree
/// base, so no derived path can climb out of it.
fn is_safe_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// Returns the worktree folder name [`session_folder`] derives for
/// `session_id`, or `None` when its first eight bytes split a character.
fn folder_name(session_id: &str) -> Option<&str> {
    session_id.get(..session_id.len().min(8))
}

/// Returns whether `path` is a real directory rather than a symbolic link
/// that could point out of the worktree base.
fn is_removable_directory(services: &AppServices, path: &Path) -> bool {
    let fs_client = services.fs_client();

    !fs_client.is_symlink(path.to_path_buf()) && fs_client.is_dir(path.to_path_buf())
}

#[cfg(test)]
#[path = "worktree_sweep_test.rs"]
mod tests;
