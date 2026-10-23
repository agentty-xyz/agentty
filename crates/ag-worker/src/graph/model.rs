use std::num::NonZeroU64;
use std::path::PathBuf;

use ag_contracts::{OneShotError, PermissionMode, ProviderCallBudget};
use ag_session::{ExecutionGraph, GraphExecutionRef, GraphNode, GraphResolvedInput};
use async_trait::async_trait;

/// Immutable execution receipt. Status remains with the operation or utility
/// run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphExecutionRecord {
    /// Existing execution authority.
    pub execution: GraphExecutionRef,
    /// Owning graph.
    pub graph_id: String,
    /// Captured immutable revision.
    pub graph_revision: NonZeroU64,
    /// Exact inputs retained across redelivery.
    pub inputs: Vec<GraphResolvedInput>,
    /// Node in the captured revision.
    pub node_id: String,
}

/// Exact persisted input and its immutable body, supplied as untrusted data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphInputValue {
    /// Complete producing response, never a latest-result lookup at execution.
    pub body: String,
    /// Named input and exact result identity.
    pub reference: GraphResolvedInput,
}

/// Work admitted atomically with authoritative gates and input capture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphDispatch {
    /// Bodies corresponding to every input in the receipt.
    pub inputs: Vec<GraphInputValue>,
    /// Immutable node definition and execution requirements.
    pub node: GraphNode,
    /// Durable authority and delivery identity.
    pub record: GraphExecutionRecord,
}

/// Admission never treats redelivery as permission to execute again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphAdmission {
    /// First and only dispatch of a static node.
    Claimed(Box<GraphDispatch>),
    /// Dependencies or conflicting Env work are unfinished.
    Blocked,
    /// An earlier call admitted this node; explicit recovery is separate.
    Existing(GraphExecutionRecord),
}

/// Atomic durable admission boundary implemented by the storage adapter.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait GraphDispatchRepository: Send + Sync {
    /// Loads the current immutable graph; missing/stale graphs are errors.
    async fn graph(&self, id: &str, revision: NonZeroU64) -> Result<ExecutionGraph, OneShotError>;

    /// Under a writer lock, rechecks revision and every dependency, pins
    /// results, creates a task attempt or utility run, and links its receipt.
    /// An existing node is acknowledged without allocating or replaying work.
    async fn claim(
        &self,
        graph_id: &str,
        revision: NonZeroU64,
        node_id: &str,
        execution_id: &str,
        folder: &std::path::Path,
    ) -> Result<GraphAdmission, OneShotError>;

    /// Saves the complete response only after authoritative successful
    /// settlement, including ordered effects. Publishes declared outputs and
    /// validates question-free responses; never accepts outputs for the user.
    async fn finish(&self, dispatch: &GraphDispatch, body: &str) -> Result<(), OneShotError>;

    /// Settles delivery/provider failures without reopening terminal work.
    async fn fail(&self, record: &GraphExecutionRecord, error: &str) -> Result<(), OneShotError>;
}

/// Enforced resource ceilings passed to a trusted worker adapter.
pub struct GraphRunRequest {
    /// Durable claim, captured definition, and exact input bodies.
    pub dispatch: GraphDispatch,
    /// Host-resolved Env directory.
    pub folder: PathBuf,
    /// Intersection of session, definition, and graph requirements.
    pub permission_mode: PermissionMode,
    /// One shared counter, including provider retries and protocol repair.
    pub provider_call_budget: Option<ProviderCallBudget>,
}

/// Host execution policy, analogous to `WorkerHost` for serial scheduling.
/// Implementations must submit through worker clients and return only after
/// authoritative settlement and ordered post-processing. Task runners retain
/// isolated task context; utility runners use the preadmitted utility ID.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait GraphNodeRunner: Send + Sync {
    /// Rejects unsupported profiles before *any* node is admitted.
    fn supports(&self, node: &GraphNode) -> bool;

    /// Executes one freshly claimed node under the executor's resource lease.
    async fn run(&self, request: GraphRunRequest) -> Result<String, OneShotError>;
}
