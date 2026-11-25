//! Commands a host runs on its own behalf inside the native sandbox, over
//! content that commands the host does not trust could have written.

use std::path::PathBuf;

use crate::{AgentError, AgentFuture, TurnNotice};

/// Runs [`SessionCommandRequest`]s inside the native sandbox their
/// [`CommandScope`] selects.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
pub trait SessionCommandClient: Send + Sync {
    /// Runs `request` to completion.
    ///
    /// # Errors
    /// Returns an error when the command cannot run at all, including when
    /// its scope refuses to run without the sandbox; a command that runs and
    /// fails is reported through its exit code instead.
    fn run(
        &self,
        request: SessionCommandRequest,
    ) -> AgentFuture<Result<SessionCommandOutput, AgentError>>;
}

/// One command a host runs on its own behalf in a folder, such as the
/// repository's Git hooks, inside the sandbox of the commands that could
/// have written whatever it reads there, so it never runs with the host's
/// own access.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionCommandRequest {
    /// Shell source Bash runs in `folder`.
    pub command: String,
    /// Folder the command runs in and may write.
    pub folder: PathBuf,
    /// Sandbox the command runs in.
    pub scope: CommandScope,
}

/// Which sandbox a [`SessionCommandRequest`] runs in.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CommandScope {
    /// The sandbox of a session's own commands: the same writable places,
    /// read-only Git metadata, network access, environment, and temporary
    /// directory. Where the native sandbox cannot run, the command runs
    /// without it and [`SessionCommandOutput::sandbox`] reports that as a
    /// session turn does.
    Session {
        /// Session whose sandbox and temporary directory the command shares.
        session_id: String,
    },
    /// The sandbox of a utility prompt's commands in the folder, with a
    /// fresh temporary directory for this command. Where the native sandbox
    /// cannot run, the command is refused with the remedy instead.
    Workspace,
}

/// What one [`SessionCommandRequest`] did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionCommandOutput {
    /// Exit code, or `None` when the command was killed or timed out.
    pub exit_code: Option<i32>,
    /// Captured standard output followed by standard error, bounded, with
    /// omitted bytes marked.
    pub output: String,
    /// [`TurnNotice::SandboxUnavailable`] when the command ran without the
    /// native sandbox, otherwise [`TurnNotice::SandboxAvailable`], so a host
    /// can report it as it reports a turn's sandbox state.
    pub sandbox: TurnNotice,
}

impl SessionCommandOutput {
    /// Returns whether the command exited with status zero.
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }
}

#[cfg(test)]
#[path = "command_test.rs"]
mod tests;
