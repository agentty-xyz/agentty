use ag_protocol::TurnPromptAttachment;
use ag_session::{GraphProposalReplyOutcome, GraphResult};
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

/// One node's prior conversation and current continuation, kept separate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphNodeHistory {
    /// Images from free-form replies up to the current execution, in order.
    pub attachments: Vec<TurnPromptAttachment>,
    /// Explicit retry, clarification, or reply addressed to this execution.
    /// Deliver as current task input, not as historical replay.
    pub continuation_prompt: Option<String>,
    /// Replayed explicit replies and settled responses, as text data.
    pub text: String,
}

/// One chat reply to an exact proposal version and its first decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphProposalReplyRecord {
    /// Caller delivery identity.
    pub id: String,
    /// Whether the decided action was attempted, or the reply was abandoned.
    pub is_settled: bool,
    /// User-authored reply text.
    pub message: String,
    /// Decision recorded for this reply, absent until decided.
    pub outcome: Option<GraphProposalReplyOutcome>,
    /// Owning proposal.
    pub proposal_id: String,
    /// Session that owns the proposal.
    pub session_id: String,
    /// Exact proposal version the reply answered.
    pub version: u32,
}
