//! Crate-wide hook runner for tests that commit in real repositories.

use std::path::PathBuf;
use std::sync::Arc;

use crate::client::GitFuture;
use crate::error::GitError;
use crate::hook::{CommitHookRunner, HookOutput, PRE_COMMIT_HOOK_COMMAND};

/// Hook runner that runs the repository's `pre-commit` hook on this
/// machine, standing in for a host's sandboxed runner.
pub(crate) struct HostHooks;

impl HostHooks {
    /// Returns the runner as a shared [`CommitHookRunner`].
    pub(crate) fn shared() -> Arc<dyn CommitHookRunner> {
        Arc::new(Self)
    }
}

impl CommitHookRunner for HostHooks {
    fn run_pre_commit(&self, repo_path: PathBuf) -> GitFuture<Result<HookOutput, GitError>> {
        Box::pin(async move {
            let output = tokio::process::Command::new("sh")
                .args(["-c", PRE_COMMIT_HOOK_COMMAND])
                .current_dir(repo_path)
                .output()
                .await
                .expect("pre-commit hook should start");

            Ok(HookOutput {
                output: format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ),
                success: output.status.success(),
            })
        })
    }
}
