//! Pure graph contracts; persistence, dispatch, and recovery remain host work.

mod error;
mod graph;
mod lifecycle;
mod node;
mod proposal;
mod proposal_state;
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
pub use proposal::{GraphProposalLimits, GraphProposalResolution};
pub use proposal_state::{
    GenerateGraphProposalRequest, GraphProposalAction, GraphProposalApproval,
    GraphProposalReplyOutcome, GraphProposalState, GraphProposalVersion,
    GraphProposalVersionOutcome,
};
pub use result::{
    GraphExecutionRef, GraphResolvedInput, GraphResult, GraphResultRef, GraphResultState,
};
