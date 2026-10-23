use std::num::NonZeroU64;

use ag_session::{GraphExecutionRef, GraphResolvedInput, GraphResult};

/// Immutable attachment to an existing attempt or utility run, not admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphExecutionRecord {
    /// Existing execution authority; its status is never copied here.
    pub execution: GraphExecutionRef,
    /// Owning graph instance.
    pub graph_id: String,
    /// Revision used for this execution.
    pub graph_revision: NonZeroU64,
    /// Every declared input resolved to a persisted exact version.
    pub inputs: Vec<GraphResolvedInput>,
    /// Node within the captured revision.
    pub node_id: String,
}

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
