//! Graph dispatch records, admission outcomes, Env locations, and the
//! repository, Env resolver, and node runner traits.

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
    /// Loads the current immutable graph for `revision`: the current revision,
    /// or an earlier one that only expansions followed. Missing graphs and
    /// other stale revisions are errors.
    async fn graph(&self, id: &str, revision: NonZeroU64) -> Result<ExecutionGraph, OneShotError>;

    /// Under a writer lock, rechecks revision and every dependency, pins
    /// results, creates a task attempt or utility run, and links its receipt.
    /// An existing node is acknowledged without allocating or replaying work.
    /// `location` is where the node's Env resolver located the run; a fresh
    /// utility run records it, a task attempt does not.
    async fn claim(
        &self,
        graph_id: &str,
        revision: NonZeroU64,
        node_id: &str,
        execution_id: &str,
        location: &GraphEnvLocation,
    ) -> Result<GraphAdmission, OneShotError>;

    /// Saves the complete response only after authoritative successful
    /// settlement, including ordered effects. Publishes declared outputs and
    /// validates question-free responses; never accepts outputs for the user.
    async fn finish(&self, dispatch: &GraphDispatch, body: &str) -> Result<(), OneShotError>;

    /// Settles delivery/provider failures without reopening terminal work.
    async fn fail(&self, record: &GraphExecutionRecord, error: &str) -> Result<(), OneShotError>;
}

/// Where one node's run happens, as the host's [`GraphEnvResolver`] located
/// it. The executor never inspects it: it passes it to the durable claim and
/// hands it to the runner unchanged, so a new kind of Env changes the
/// resolver, the repository, and the runners, never the executor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphEnvLocation {
    /// A directory on this host.
    Folder(PathBuf),
}

/// Host port that places each node in its Env around durable admission.
///
/// For every node it tries to admit, the executor first reserves the node's
/// first provider call, then:
///
/// 1. `locate` returns where the node would run, or nothing while its Env
///    cannot be offered, which leaves the node unclaimed for a later pass, as
///    an exhausted budget does.
/// 2. `claim` receives that location. A blocked or already admitted node
///    creates no execution, and the location is simply dropped.
/// 3. `prepare` makes the location ready for an execution `claim` just created,
///    before its runner starts.
///
/// The port holds no reservation, so nothing needs releasing when a claim
/// creates no execution. The in-memory Env lease in
/// [`crate::GraphResources`] is separate and still keyed by the node's
/// `EnvId`.
///
/// A host that provisions an Env per node keeps the provisioning durable
/// outside this port:
///
/// - It reserves the Env inside the claim transaction, keyed by graph and node,
///   so a limit is checked atomically with admission and a blocked or finished
///   node writes nothing.
/// - It takes the session status lease before preparation, not only when the
///   runner starts, so a slow preparation still shows the session busy.
/// - Its conflict rule attributes a node's child utility runs to that node's
///   Env rather than to the session worktree.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait GraphEnvResolver: Send + Sync {
    /// Returns where node `node` of graph `graph_id` would run, or `None`
    /// while its Env cannot be offered now.
    ///
    /// It performs no durable effect and must not wait for an Env to become
    /// available. When an Env it refused may be available again, the host
    /// wakes the graph's drivers, which locate the node on their next pass.
    ///
    /// # Errors
    /// Fails the dispatch pass before anything is admitted, as a storage
    /// failure in `claim` does.
    async fn locate(
        &self,
        graph_id: &str,
        node: &GraphNode,
    ) -> Result<Option<GraphEnvLocation>, OneShotError>;

    /// Makes `location` ready for `dispatch`, the execution just claimed
    /// there, before its runner starts. A live executor calls it at most once
    /// per claim and never for redelivery; a claim whose process stopped
    /// before preparation is prepared when it is retried.
    ///
    /// # Errors
    /// The executor records the error as the execution's failure, through
    /// the same durable path as a runner failure.
    async fn prepare(
        &self,
        dispatch: &GraphDispatch,
        location: &GraphEnvLocation,
    ) -> Result<(), OneShotError>;
}

/// Enforced resource ceilings passed to a trusted worker adapter.
pub struct GraphRunRequest {
    /// Durable claim, captured definition, and exact input bodies.
    pub dispatch: GraphDispatch,
    /// Prepared Env location, the same one the claim received.
    pub location: GraphEnvLocation,
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

    /// Serial session adapters acquire the Env lease after selecting the
    /// command, so an earlier command never waits behind a queued node's lease.
    /// Such adapters must retain that lease through authoritative settlement.
    fn manages_environment(&self, node: &GraphNode) -> bool {
        let _ = node;
        false
    }

    /// Executes one freshly claimed node under the executor's resource lease.
    async fn run(&self, request: GraphRunRequest) -> Result<String, OneShotError>;
}
