//! Commit hooks run outside the host Git process.
//!
//! Host Git runs no hooks (see `repo::host_configuration`): a hook, or the
//! configuration a hook manager reads, can come from the session worktree,
//! which commands the host does not trust can write. Commit workflows
//! instead run the `pre-commit` hook through a [`CommitHookRunner`] that
//! the host chooses, such as one that runs it inside the session's sandbox,
//! and then commit with hooks disabled.

use std::path::PathBuf;

use super::client::GitFuture;
use super::error::GitError;

/// Shell command that runs the repository's effective `pre-commit` hook
/// against the current index, succeeding when none is configured, exactly
/// as `git commit` would run it.
pub const PRE_COMMIT_HOOK_COMMAND: &str = "git hook run --ignore-missing pre-commit";

/// Command label of errors from running the `pre-commit` hook.
pub const PRE_COMMIT_HOOK_LABEL: &str = "git hook run pre-commit";

/// Runs a repository's commit hooks with the access the host chooses.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
pub trait CommitHookRunner: Send + Sync {
    /// Runs [`PRE_COMMIT_HOOK_COMMAND`] in `repo_path` and returns what the
    /// hook did.
    ///
    /// # Errors
    /// Returns an error when the hook cannot be run at all; a hook that runs
    /// and rejects the changes is reported through [`HookOutput`] instead.
    fn run_pre_commit(&self, repo_path: PathBuf) -> GitFuture<Result<HookOutput, GitError>>;
}

/// What one hook run did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookOutput {
    /// Captured output of the hook, for the user and for commit assistance.
    pub output: String,
    /// Whether the hook accepted the changes.
    pub success: bool,
}

impl HookOutput {
    /// Returns whether the hook rejected the changes only because it
    /// rewrote files, such as a formatter, so restaging and running it again
    /// can succeed.
    pub(crate) fn modified_files(&self) -> bool {
        !self.success
            && self
                .output
                .to_ascii_lowercase()
                .contains("files were modified by this hook")
    }

    /// Returns the error a rejected commit reports, carrying the hook's
    /// output as `git commit` would.
    pub(crate) fn rejection(&self, command: &str) -> GitError {
        GitError::CommandFailed {
            command: command.to_string(),
            stderr: self.output.trim().to_string(),
        }
    }
}

#[cfg(test)]
#[path = "hook_test.rs"]
mod tests;
