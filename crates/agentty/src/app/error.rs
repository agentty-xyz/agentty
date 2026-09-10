use crate::app::session::SessionError;

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
    OneShot(#[from] ag_agent::OneShotError),

    /// A workflow-specific or startup failure with a contextual message.
    #[error("{0}")]
    Workflow(String),
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
