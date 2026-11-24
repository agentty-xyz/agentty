//! Commit hooks run inside the native sandbox instead of on the host.
//!
//! A repository's `pre-commit` hook, and the configuration a hook manager
//! reads, such as `.pre-commit-config.yaml` or a `core.hooksPath` inside the
//! checkout, come from files that model-run commands can write: a session's
//! commands write its worktree, and conflict assistance writes resolutions
//! in the project checkout during sync. Running them with the host's access
//! would let those commands run code outside their sandbox, so commit
//! workflows run them through [`SandboxedCommitHookRunner`] and Git commits
//! on the host with hooks disabled.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_contracts::{CommandScope, SessionCommandClient, SessionCommandRequest, TurnNotice};
use ag_git::{
    CommitHookRunner, GitError, GitFuture, HookOutput, PRE_COMMIT_HOOK_COMMAND,
    PRE_COMMIT_HOOK_LABEL,
};
use tokio::sync::mpsc;

use super::sandbox::{SandboxNoticeTarget, SandboxNotices};
use crate::app::AppEvent;
use crate::app::service::SessionUpdateVersionMap;
use crate::domain::session::SessionId;
use crate::domain::session_message::SessionTranscript;
use crate::infra::db::AppRepositories;

/// Runs a repository's commit hooks inside the native sandbox of the
/// commands that could have written them.
pub(super) struct SandboxedCommitHookRunner {
    /// Runs the hook command inside the native sandbox.
    client: Arc<dyn SessionCommandClient>,
    /// Sandbox the hook runs in.
    scope: HookScope,
}

impl SandboxedCommitHookRunner {
    /// Returns a runner of hooks in `session`'s worktree inside the sandbox
    /// of the session's own commands, reporting that sandbox's state to the
    /// session transcript as a turn does.
    pub(super) fn session(
        client: Arc<dyn SessionCommandClient>,
        session: SessionHookTarget,
    ) -> Self {
        Self {
            client,
            scope: HookScope::Session(session),
        }
    }

    /// Returns a runner of hooks in a project checkout inside a utility
    /// sandbox scoped to it, refusing where the native sandbox cannot run.
    pub(super) fn workspace(client: Arc<dyn SessionCommandClient>) -> Self {
        Self {
            client,
            scope: HookScope::Workspace,
        }
    }
}

impl CommitHookRunner for SandboxedCommitHookRunner {
    fn run_pre_commit(&self, repo_path: PathBuf) -> GitFuture<Result<HookOutput, GitError>> {
        let client = Arc::clone(&self.client);
        let request = SessionCommandRequest {
            command: PRE_COMMIT_HOOK_COMMAND.to_string(),
            folder: repo_path,
            scope: self.scope.command_scope(),
        };
        let session = self.scope.session().cloned();

        Box::pin(async move {
            let output = client
                .run(request)
                .await
                .map_err(|error| GitError::CommandFailed {
                    command: PRE_COMMIT_HOOK_LABEL.to_string(),
                    stderr: error.to_string(),
                })?;
            if let Some(session) = session {
                session.report(&output.sandbox).await;
            }

            Ok(HookOutput {
                success: output.succeeded(),
                output: output.output,
            })
        })
    }
}

/// The session whose worktree hooks run in and whose transcript receives
/// the sandbox notices of hook runs.
#[derive(Clone)]
pub(super) struct SessionHookTarget {
    /// Reducer event sender notified after a notice is appended.
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Repository bundle persisting appended notices.
    pub(super) db: AppRepositories,
    /// Session whose sandbox hook runs share.
    pub(super) session_id: SessionId,
    /// Per-app session update versions shared with the main runtime.
    pub(super) session_update_versions: SessionUpdateVersionMap,
    /// Shared typed transcript snapshot mirrored to the render layer.
    pub(super) transcript: Arc<Mutex<SessionTranscript>>,
}

impl SessionHookTarget {
    /// Appends the notice a hook run's sandbox state adds to the session,
    /// as a turn's notice would: a warning unless the latest one is the
    /// same, and a note when the sandbox works again after one.
    async fn report(&self, notice: &TurnNotice) {
        SandboxNoticeTarget {
            app_event_tx: &self.app_event_tx,
            db: &self.db,
            session_id: &self.session_id,
            session_update_versions: &self.session_update_versions,
            transcript: &self.transcript,
        }
        .report(&mut SandboxNotices::default(), notice)
        .await;
    }
}

/// Which sandbox hook runs use.
enum HookScope {
    /// The sandbox of a session's own commands.
    Session(SessionHookTarget),
    /// A utility sandbox scoped to the checkout the hook runs in.
    Workspace,
}

impl HookScope {
    /// Returns the command scope hook runs request.
    fn command_scope(&self) -> CommandScope {
        match self {
            Self::Session(session) => CommandScope::Session {
                session_id: session.session_id.to_string(),
            },
            Self::Workspace => CommandScope::Workspace,
        }
    }

    /// Returns the session receiving sandbox notices, if any.
    fn session(&self) -> Option<&SessionHookTarget> {
        match self {
            Self::Session(session) => Some(session),
            Self::Workspace => None,
        }
    }
}

#[cfg(test)]
#[path = "commit_hook_test.rs"]
mod tests;
