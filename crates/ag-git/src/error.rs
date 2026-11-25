//! `GitError` categories returned by git infrastructure operations.

use std::time::Duration;

use crate::rebase;

/// Typed error returned by git infrastructure operations.
///
/// Wraps command execution failures, output parsing issues, and I/O errors so
/// callers can distinguish error categories without parsing opaque strings.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// A git subprocess exited with a non-zero status.
    #[error("{command}: {stderr}")]
    CommandFailed {
        /// The git command that was executed (e.g. `"git rebase main"`).
        command: String,
        /// Human-readable detail extracted from stderr/stdout.
        stderr: String,
    },

    /// A git subprocess exceeded its configured runtime bound.
    #[error("{command} timed out after {timeout:?}")]
    CommandTimedOut {
        /// Git invocation that exceeded the timeout.
        command: String,
        /// Configured command timeout.
        timeout: Duration,
    },

    /// Git command output could not be parsed into the expected structure.
    #[error("{0}")]
    OutputParse(String),

    /// The requested repository or worktree is no longer available.
    #[error("{detail}")]
    RepositoryUnavailable {
        /// Original repository-discovery failure detail.
        detail: String,
    },

    /// A filesystem or process-spawn operation failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),

    /// A repository declares pre-commit validation but its Git hook is
    /// unavailable.
    #[error(
        "pre-commit validation is configured by `{config_file}`, but the Git pre-commit hook is \
         not installed or executable. Install it with one of these commands:\n\n  prek install\n  \
         pre-commit install\n\nAgentty will continue for now, but missing configured hooks will \
         become an error in a future release."
    )]
    PreCommitHookMissing {
        /// Repository-root-relative configuration file that declares
        /// validation.
        config_file: String,
    },

    /// A Git repository sits inside the worktree where Agentty's own Git
    /// would run its configuration, such as one a session committed at a
    /// submodule path, so Agentty refuses to work on the worktree.
    #[error(
        "`{path}` holds a Git repository that the session left in its worktree; Agentty does not \
         stage, commit, or rebase the worktree while it is there. Remove it, or move it out of \
         the worktree, then try again."
    )]
    EmbeddedRepository {
        /// Worktree-relative path of the repository.
        path: String,
    },

    /// A `tokio::task::spawn_blocking` join failed.
    #[error("Join error: {0}")]
    Join(#[from] tokio::task::JoinError),
}

/// Lowercase stderr fragments that identify a rejected or missing credential.
const AUTH_FAILURE_MARKERS: [&str; 10] = [
    "access denied",
    "authentication failed",
    "could not read password",
    "could not read username",
    "host key verification failed",
    "invalid username or password",
    "permission denied (",
    "returned error: 401",
    "returned error: 403",
    "terminal prompts disabled",
];

/// Lowercase stderr fragments that identify an unreachable remote host.
const NETWORK_FAILURE_MARKERS: [&str; 10] = [
    "connection closed by",
    "connection refused",
    "connection reset",
    "connection timed out",
    "could not resolve host",
    "failed to connect to",
    "kex_exchange_identification",
    "network is unreachable",
    "no route to host",
    "operation timed out",
];

impl GitError {
    /// Returns a bounded, content-free failure class for telemetry.
    ///
    /// The class is one of `auth` (rejected or missing credentials),
    /// `network` (unreachable remote), `timeout` (runtime bound exceeded), or
    /// `other`. It is derived from the error kind and well-known Git and SSH
    /// diagnostics, so callers can export it without exporting stderr text,
    /// paths, or remote URLs.
    #[must_use]
    pub fn failure_class(&self) -> &'static str {
        match self {
            Self::CommandTimedOut { .. } => "timeout",
            Self::CommandFailed { stderr, .. } => {
                let stderr = stderr.to_lowercase();
                let contains_any =
                    |markers: &[&str]| markers.iter().any(|marker| stderr.contains(marker));

                if contains_any(&AUTH_FAILURE_MARKERS) {
                    "auth"
                } else if contains_any(&NETWORK_FAILURE_MARKERS) {
                    "network"
                } else {
                    "other"
                }
            }
            _ => "other",
        }
    }

    /// Returns whether a failed command reports Git index-lock contention.
    ///
    /// This identifies the lock failure without implying that the lock is
    /// stale or safe to remove.
    #[must_use]
    pub fn is_index_locked(&self) -> bool {
        matches!(self, Self::CommandFailed { stderr, .. } if rebase::is_git_index_lock_error(stderr))
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
