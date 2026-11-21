//! Durable immutable graph snapshots, execution links, and exact result
//! evidence.

mod budget;
mod dispatch;
mod execution;
mod lifecycle;
mod model;
mod output_reply;
mod progress;
mod proposal;
mod proposal_reply;
mod repository;
mod result;
mod task_turn;

pub use model::{
    GRAPH_NODE_ACTIVITY_OPERATION_KIND, GraphEvidence, GraphExecutionRecord, GraphNodeHistory,
    GraphOutputReplyRecord, GraphProposalReplyRecord, StoredGraphEvidence, StoredGraphResult,
};
pub use proposal::GraphProposalRepository;
#[cfg(any(test, feature = "test-utils"))]
pub use proposal::MockGraphProposalRepository;
pub use repository::GraphRepository;
#[cfg(any(test, feature = "test-utils"))]
pub use repository::MockGraphRepository;
pub(crate) use repository::SqliteGraphRepository;

#[cfg(test)]
#[path = "execution_graph/support_test.rs"]
mod support_test;

#[cfg(test)]
#[path = "execution_graph/store_test.rs"]
mod tests;
