/// Typed error returned by session-layer workflow operations.
///
/// Wraps infrastructure errors from git, database, app-server, and forge
/// boundaries alongside workflow-specific validation failures, replacing the
/// previous opaque `Result<T, String>` pattern used throughout session
/// orchestration code.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Session not found in the current manager state.
    #[error("Session not found")]
    NotFound,

    /// Session runtime handles not found.
    #[error("Session handles not found")]
    HandlesNotFound,

    /// A git infrastructure operation failed.
    #[error("{0}")]
    Git(#[from] ag_git::GitError),

    /// An isolated agent prompt failed.
    #[error("{0}")]
    OneShot(#[from] ag_agent::OneShotError),

    /// A database operation failed.
    #[error("{0}")]
    Db(#[from] crate::infra::db::DbError),

    /// A filesystem boundary operation failed.
    #[error("{0}")]
    Fs(#[from] crate::infra::fs::FsError),

    /// An app-server operation failed.
    #[error("{0}")]
    AppServer(#[from] ag_agent::AppServerError),

    /// A workflow-specific failure with a contextual message.
    ///
    /// Covers validation, forge operations, template rendering, and other
    /// transient or domain-specific failures that do not warrant dedicated
    /// variants.
    #[error("{0}")]
    Workflow(String),

    /// The user explicitly stopped the active turn before the agent finished.
    #[error("{0}")]
    StoppedByUser(String),
}

impl SessionError {
    /// Prefixes the display message of `Workflow` variants with the given
    /// context string so callers can distinguish which assist operation
    /// produced the failure.
    ///
    /// `StoppedByUser` keeps the same typed routing while adding context to
    /// the display message. Typed infrastructure variants (`Git`, `Db`,
    /// `AppServer`) pass through unchanged because their type already
    /// identifies the failure origin and callers can still discriminate them
    /// by pattern matching. In practice every current assist call site
    /// (`run_rebase_assist_agent`, `run_sync_rebase_assist_agent`,
    /// commit-assist in `task.rs`) only receives `Workflow` variants because
    /// the upstream assist functions convert all errors via
    /// `SessionError::Workflow(String)`. The pass-through arm is a safety net
    /// so future callers that propagate typed infra errors do not silently
    /// lose their structured variant.
    #[must_use]
    pub fn with_context(self, context: &str) -> Self {
        match self {
            Self::Workflow(message) => Self::Workflow(format!("{context}: {message}")),
            Self::StoppedByUser(message) => Self::StoppedByUser(format!("{context}: {message}")),
            other => other,
        }
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
