//! Headless scheduling and lifecycle coordination for agent runs.
//!
//! Hosts supply workflow policy, runtime configuration, and persistence. The
//! worker owns serial dispatch and cancellation; it does not depend on a
//! frontend, provider implementation, Git, or SQLite.

mod graph;
mod lifecycle;
mod mailbox;
mod provider;
mod run;
mod scheduler;
mod scope;
mod store;
mod turn;
mod utility;

/// Host contract: a host whose runtime configuration enables harness
/// sessions calls this first in `main`, before starting any runtime or
/// thread, and returns its exit code at once when it is `Some`; the
/// executable then serves as the native sandbox helper of its sessions.
/// Without the call, their commands run unsandboxed and every turn warns.
pub use ag_runtime::dispatch_sandbox_helper;
pub use graph::{
    GraphAdmission, GraphDispatch, GraphDispatchRepository, GraphExecutionRecord, GraphExecutor,
    GraphInputValue, GraphNodeRunner, GraphResources, GraphRunRequest, GraphUtilityRunner,
};
pub use lifecycle::{Clock, HeartbeatClock, execute, recover};
#[cfg(any(test, feature = "test-utils"))]
pub use mailbox::test_session_worker_handle;
pub use mailbox::{SessionWorkerHandle, SessionWorkerPause, SessionWorkerTask};
pub use provider::{
    RealAgentAvailabilityProbe, RuntimeConfig, cleanup_session_worktree_artifacts, setup_backend,
    supports_turn_steering, uses_persistent_session,
};
pub use run::{RunInfo, RunRepository, RunState};
pub use scheduler::{ScheduledCommand, ScheduledWork, WorkQueue, WorkerHost, next_work, run};
pub use scope::{RunScope, in_scope, scoped_client, with_execution_resource};
#[cfg(any(test, feature = "test-utils"))]
pub use store::MockOperationRepository;
pub use store::{OperationRepository, SessionOperationRow};
pub use turn::SessionRunClient;
#[cfg(any(test, feature = "test-utils"))]
pub use utility::MockRunClient;
pub use utility::{RunClient, RunWorker};

#[cfg(any(test, feature = "test-utils"))]
#[path = "adapter_support_test.rs"]
pub mod test_support;
