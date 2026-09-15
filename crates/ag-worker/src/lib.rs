//! Headless scheduling and lifecycle coordination for agent runs.
//!
//! Hosts supply workflow policy, runtime adapters, and persistence. The worker
//! owns serial dispatch and cancellation; it does not depend on a frontend,
//! provider implementation, Git, or SQLite.

mod lifecycle;
mod scheduler;
mod store;
mod turn;

pub use lifecycle::{Clock, HeartbeatClock, execute, recover};
pub use scheduler::{ScheduledCommand, ScheduledWork, WorkQueue, WorkerHost, next_work, run};
#[cfg(any(test, feature = "test-utils"))]
pub use store::MockOperationRepository;
pub use store::{OperationRepository, SessionOperationRow};
pub use turn::run_turn;
