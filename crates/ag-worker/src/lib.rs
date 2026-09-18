//! Headless scheduling and lifecycle coordination for agent runs.
//!
//! Hosts supply workflow policy, runtime configuration, and persistence. The
//! worker owns serial dispatch and cancellation; it does not depend on a
//! frontend, provider implementation, Git, or SQLite.

mod lifecycle;
mod mailbox;
mod provider;
mod run;
mod scheduler;
mod scope;
mod store;
mod turn;
mod utility;

pub use lifecycle::{Clock, HeartbeatClock, execute, recover};
#[cfg(any(test, feature = "test-utils"))]
pub use mailbox::test_session_worker_handle;
pub use mailbox::{SessionWorkerHandle, SessionWorkerPause};
pub use provider::{
    RealAgentAvailabilityProbe, RuntimeConfig, cleanup_session_worktree_artifacts, setup_backend,
    uses_persistent_session,
};
pub use run::{RunInfo, RunRepository, RunState};
pub use scheduler::{ScheduledCommand, ScheduledWork, WorkQueue, WorkerHost, next_work, run};
pub use scope::{RunScope, in_scope, scoped_client};
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
