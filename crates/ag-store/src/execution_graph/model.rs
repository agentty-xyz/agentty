use ag_protocol::{TurnPrompt, TurnPromptAttachment};
use ag_session::{GraphExecutionRef, GraphResult, TaskChainReplyOutcome};
pub use ag_worker::GraphExecutionRecord;

/// Trusted host evidence for an exact result, supplied separately from output.
///
/// This boundary is for host adapters. Model output must never be converted
/// directly into validation or user-acceptance evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphEvidence {
    /// Validator identity or explicitly accepting initiator.
    pub actor: String,
    /// Validation performed or explicit acceptance action being recorded.
    pub reason: String,
}

/// Once-only evidence with an injected storage timestamp.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredGraphEvidence {
    /// Original host attestation; conflicting redelivery is rejected.
    pub evidence: GraphEvidence,
    /// First successful write time, in Unix seconds.
    pub recorded_at: i64,
}

/// Consistent output snapshot; lifecycle still belongs to its execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredGraphResult {
    /// Explicit acceptance of this validated version, when recorded.
    pub acceptance: Option<StoredGraphEvidence>,
    /// Exact immutable response body under the referenced protocol schema.
    pub body: String,
    /// Exact reference and state projected from the evidence ledger.
    pub result: GraphResult,
    /// Host validation after successful, question-free settlement.
    pub validation: Option<StoredGraphEvidence>,
}

/// One node's replayed conversation before its current execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphNodeHistory {
    /// Images from free-form replies up to the current execution, in order.
    pub attachments: Vec<TurnPromptAttachment>,
    /// Replayed explicit replies and settled responses, as text data.
    pub text: String,
}

/// One natural-language task-chain reply and its first recorded decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskChainReplyRecord {
    /// Continuation identity chosen with the decision, if it continues work.
    pub action_id: Option<String>,
    /// Owning task-chain graph.
    pub chain_id: String,
    /// Decision recorded for this reply, absent until decided.
    pub outcome: Option<TaskChainReplyOutcome>,
    /// Visible user prompt, including attachments.
    pub prompt: TurnPrompt,
    /// Exact execution the reply answered when it was recorded.
    pub source: GraphExecutionRef,
}
