//! Headless scheduling and lifecycle coordination for agent runs.
//!
//! Hosts supply workflow policy, runtime adapters, and persistence. The worker
//! owns serial dispatch and cancellation; it does not depend on a frontend,
//! provider implementation, Git, or SQLite.

mod lifecycle;
mod run;
mod scheduler;
mod scope;
mod store;
mod turn;
mod utility;

pub use lifecycle::{Clock, HeartbeatClock, execute, recover};
pub use run::{RunInfo, RunRepository, RunState};
pub use scheduler::{ScheduledCommand, ScheduledWork, WorkQueue, WorkerHost, next_work, run};
pub use scope::{RunScope, in_scope, scoped_client};
#[cfg(any(test, feature = "test-utils"))]
pub use store::MockOperationRepository;
pub use store::{OperationRepository, SessionOperationRow};
pub use turn::run_turn;
#[cfg(any(test, feature = "test-utils"))]
pub use utility::MockRunClient;
pub use utility::{RunClient, RunWorker};
