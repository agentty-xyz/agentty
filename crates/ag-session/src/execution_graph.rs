//! Pure graph contracts; persistence, dispatch, and recovery remain host work.

mod error;
mod graph;
mod node;
mod result;

#[cfg(test)]
#[path = "execution_graph/test_support_test.rs"]
mod test_support;

pub use error::GraphContractError;
pub use graph::ExecutionGraph;
pub use node::{
    GraphCompletionGate, GraphContext, GraphDependency, GraphExecutionProfile, GraphInputBinding,
    GraphNode, GraphOutput, GraphPostProcessing, GraphWorkspaceAccess,
};
pub use result::{
    GraphExecutionRef, GraphResolvedInput, GraphResult, GraphResultRef, GraphResultState,
};
