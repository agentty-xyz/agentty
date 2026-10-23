//! Deterministic static graph dispatch through host-owned worker adapters.

mod executor;
mod model;
mod utility;

pub use executor::{GraphExecutor, GraphResources};
pub use model::{
    GraphAdmission, GraphDispatch, GraphDispatchRepository, GraphExecutionRecord, GraphInputValue,
    GraphNodeRunner, GraphRunRequest,
};
pub use utility::GraphUtilityRunner;

#[cfg(test)]
#[path = "graph/support_test.rs"]
mod support_test;
