//! Runtime composition and dispatch for worker-owned agent execution.
//!
//! Only `ag-worker` consumes this implementation. Shared execution contracts
//! live in `ag-contracts`; concrete provider adapters live in `ag-agent`.

mod provider;
mod runtime;

pub use provider::{
    RealAgentAvailabilityProbe, cleanup_session_worktree_artifacts, setup_backend,
    uses_persistent_session,
};
#[cfg(feature = "test-utils")]
pub use runtime::test_support;
pub use runtime::{RuntimeFactory, SessionRuntime, UtilityRuntime};
