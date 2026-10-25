//! Pure graph contracts; persistence, dispatch, and recovery remain host work.

mod error;
mod graph;
mod lifecycle;
mod node;
mod result;
mod selection;

#[cfg(test)]
#[path = "execution_graph/test_support_test.rs"]
mod test_support;

pub use error::GraphContractError;
pub use graph::ExecutionGraph;
pub use lifecycle::{
    ContinueGraphNodeRequest, ExecutionGraphProgress, ExecutionGraphStatus, GraphBlockedReason,
    GraphControlRequest, GraphNodeContinuation, GraphNodeProgress, GraphNodeStatus,
    GraphQuestionAnswer, ResumeExecutionGraphRequest,
};
pub use node::{
    GraphCompletionGate, GraphContext, GraphDependency, GraphExecutionProfile, GraphInputBinding,
    GraphNode, GraphOutput, GraphPostProcessing, GraphWorkspaceAccess,
};
pub use result::{
    GraphExecutionRef, GraphResolvedInput, GraphResult, GraphResultRef, GraphResultState,
};
