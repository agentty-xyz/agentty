use std::path::PathBuf;

use ag_contracts::OneShotError;
use async_trait::async_trait;

/// Durable identity and ownership of one isolated model execution.
#[derive(Clone, Debug)]
pub struct RunInfo {
    /// Repository where the harness executes, including project-only work.
    pub folder: PathBuf,
    /// Unique run identity; provider retries remain attempts of this run.
    pub id: String,
    /// Enclosing workflow operation, when this is a child step.
    pub parent_id: Option<String>,
    /// Owning project, when known by the host.
    pub project_id: Option<i64>,
    /// Host purpose, such as title generation or conflict assistance.
    pub purpose: String,
    /// Owning session, when the work belongs to one.
    pub session_id: Option<String>,
}

/// Persisted execution state of a worker-owned utility run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunState {
    /// Accepted and waiting for execution capacity.
    Queued,
    /// The runtime is executing the request.
    Running,
    /// The runtime returned a successful result.
    Completed,
    /// The runtime or worker failed.
    Failed,
    /// The caller, parent, or application canceled execution.
    Canceled,
}

impl RunState {
    /// Stable storage representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}

/// Persistence boundary for session-owned and project-owned utility runs.
/// Hosts recover abandoned records only after acquiring exclusive ownership.
#[async_trait]
pub trait RunRepository: Send + Sync {
    /// Records acceptance before the runtime may be invoked. Atomically rejects
    /// sessions whose admission was closed, even after their host record is
    /// deleted.
    async fn create(&self, run: &RunInfo) -> Result<(), OneShotError>;
    /// Permanently closes utility admission for a session identity. Idempotent;
    /// hosts must not reuse session IDs. Closure survives session deletion.
    async fn close_session(&self, session_id: &str) -> Result<(), OneShotError>;
    /// Changes unfinished state; terminal records must never be resurrected.
    async fn transition(
        &self,
        id: &str,
        state: RunState,
        error: Option<&str>,
    ) -> Result<(), OneShotError>;
    /// Refreshes liveness only for a running record.
    async fn heartbeat(&self, id: &str) -> Result<(), OneShotError>;
    /// Fails abandoned records after exclusive startup recovery.
    async fn recover(&self) -> Result<(), OneShotError>;
}
