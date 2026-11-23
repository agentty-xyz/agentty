//! Top-level `AppError` type for app orchestration failures.

use std::io;

use crate::app::session::SessionError;
use crate::infra::platform::PlatformError;

/// Typed error returned by top-level app orchestration operations.
///
/// Wraps session-layer errors, direct infrastructure failures from the app
/// layer, and startup or workflow-specific failures, replacing the previous
/// opaque `Result<T, String>` pattern in `App` methods and `main.rs`.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// A session workflow operation failed.
    #[error("{0}")]
    Session(#[from] SessionError),

    /// A database operation failed at the app layer.
    #[error("{0}")]
    Db(#[from] crate::infra::db::DbError),

    /// A git operation failed at the app layer.
    #[error("{0}")]
    Git(#[from] ag_git::GitError),

    /// An isolated agent prompt failed at the app layer.
    #[error("{0}")]
    OneShot(#[from] ag_contracts::OneShotError),

    /// Startup found neither a supported backend CLI nor an available
    /// in-process Harness.
    #[error(
        "No supported backend CLI found on `PATH`. Install `codex`, `claude`, or Antigravity CLI \
         1.5.8 or newer. For an older `agy`, run `agy update`, then restart `agentty`. To use the \
         built-in `ag-harness` instead, set a provider key such as `MODEL_API_KEY` or serve a \
         model from a local Ollama server."
    )]
    NoAgentAvailable,

    /// The terminal could not be prepared for the terminal UI.
    #[error("Failed to start terminal UI: {0}")]
    TerminalSetup(#[source] io::Error),

    /// The running terminal UI failed after its terminal was prepared.
    #[error("Failed to run terminal UI: {0}")]
    Terminal(#[source] io::Error),

    /// A workflow-specific or startup failure with a contextual message.
    #[error("{0}")]
    Workflow(String),
}

impl From<PlatformError> for AppError {
    /// Reports at startup that no client for the configured platform could
    /// be started.
    fn from(error: PlatformError) -> Self {
        Self::Workflow(format!("Cannot start the platform client: {error}"))
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
