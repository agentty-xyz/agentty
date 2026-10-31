use std::collections::BTreeMap;
use std::num::NonZeroU64;

use ag_protocol::TurnPromptAttachment;
use serde::{Deserialize, Serialize};

use super::{GraphCompletionGate, GraphExecutionRef};

/// Revision-bound graph control; cancellation is terminal for this graph ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphControlRequest {
    /// Owning graph identity.
    pub graph_id: String,
    /// Expected current immutable revision.
    pub revision: NonZeroU64,
}

/// Explicit restart of dispatch, never an implicit retry of an admitted node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumeExecutionGraphRequest {
    /// Graph to resume.
    pub graph: GraphControlRequest,
    /// Fresh limits after a process restart. A retained live executor keeps
    /// its remaining credits and requires its original limits.
    pub provider_call_limits: BTreeMap<String, usize>,
}

/// An explicit action against the exact latest execution of a node.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContinueGraphNodeRequest {
    /// New attempt/run UUID, also the idempotency identity of this action.
    pub action_id: String,
    /// Exact source execution, including its task and operation identities.
    pub execution: GraphExecutionRef,
    /// Owning graph.
    pub graph_id: String,
    /// Requested continuation.
    pub kind: GraphNodeContinuation,
    /// Node to continue.
    pub node_id: String,
    /// Expected graph revision.
    pub revision: NonZeroU64,
}

/// Provider retries stay inside an attempt; these actions create a new one.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum GraphNodeContinuation {
    /// Retry only a failed, canceled, or interrupted execution.
    Retry,
    /// Answer the complete ordered question set from a settled task turn.
    Answer {
        /// Ordered answers paired with their exact question text.
        answers: Vec<GraphQuestionAnswer>,
    },
    /// Free-form user guidance for the same task conversation. It may retry
    /// failed or canceled work, answer clarification questions in prose,
    /// revise a validated output that has not been accepted, or follow up on
    /// settled work. Accepted outputs and nodes whose dependents have started
    /// are final.
    Reply {
        /// Images accompanying the guidance, delivered with later attempts.
        attachments: Vec<TurnPromptAttachment>,
        /// User-authored guidance.
        message: String,
    },
}

/// One answer to the exact question produced by the source execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphQuestionAnswer {
    /// User-supplied response.
    pub answer: String,
    /// Exact question text, in its original position.
    pub question: String,
}

/// Read-only projection; operation/run records remain execution authorities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionGraphProgress {
    /// Graph identity.
    pub graph_id: String,
    /// Declaration-order node progress, including all execution identities.
    pub nodes: Vec<GraphNodeProgress>,
    /// Current immutable revision.
    pub revision: NonZeroU64,
    /// Owning session.
    pub session_id: crate::SessionId,
    /// Aggregate execution state, independent of desired-outcome acceptance.
    pub status: ExecutionGraphStatus,
}

/// Graph completion means every node settled without clarification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionGraphStatus {
    /// Dispatch is eligible or work is queued/running.
    Running,
    /// Prerequisites, acceptance, clarification, or resources prevent progress.
    Blocked,
    /// No live driver; an explicit resume is required.
    Paused,
    /// Every node settled successfully without pending questions.
    Completed,
    /// At least one node requires an explicit retry.
    Failed,
    /// Durable cancellation closed admission, including unstarted nodes.
    Cancelled,
}

/// One node's latest state plus immutable attempt/run history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphNodeProgress {
    /// Exact unsatisfied requirements for pending work.
    pub blocked: Vec<GraphBlockedReason>,
    /// Every admitted execution in order; retries append rather than replace.
    pub executions: Vec<GraphExecutionRef>,
    /// Last authoritative failure, if any.
    pub last_error: Option<String>,
    /// Node identity.
    pub node_id: String,
    /// Questions from the latest settled response only.
    pub questions: Vec<crate::QuestionItem>,
    /// Outputs produced by the latest execution, with their evidence state.
    pub results: Vec<super::GraphResult>,
    /// State projected from the latest operation/run and settled response.
    pub status: GraphNodeStatus,
}

/// Node state never overwrites authoritative operation/run status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphNodeStatus {
    /// Not admitted; inspect blocked reasons.
    Pending,
    /// Durable work is waiting for worker selection.
    Queued,
    /// Worker owns execution or ordered effects.
    Running,
    /// Settled task turn requires a complete clarification reply.
    AwaitingAnswers,
    /// Settled response has no pending questions.
    Succeeded,
    /// Failed execution or incomplete settlement requires explicit retry.
    Failed,
    /// This execution or its graph was canceled.
    Cancelled,
}

/// Stable reasons why a pending node cannot currently dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphBlockedReason {
    /// A declared predecessor gate has not been satisfied.
    Dependency {
        /// Required gate.
        gate: GraphCompletionGate,
        /// Predecessor node.
        node_id: String,
    },
    /// Conflicting durable Env work is still settling.
    EnvironmentBusy,
    /// Host capacity or a live Env lease must become available.
    AwaitingDispatch,
    /// Named shared provider-call budget has no remaining credit.
    BudgetExhausted(String),
    /// Explicit resume is required after the driver stops or restarts.
    ResumeRequired,
}
