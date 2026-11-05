//! Durable proposal inspection and explicit action contracts.

use ag_protocol::{GraphProposalResponse, TurnPrompt};
use serde::{Deserialize, Serialize};

use super::GraphProposalLimits;
use crate::{
    AgentDefinition, AgentSelection, PermissionMode, SessionId, StartExecutionGraphRequest,
};

/// Idempotent generation admission; the host captures all other policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GenerateGraphProposalRequest {
    /// Caller delivery identity, reused on retries.
    pub id: String,
    /// Original user request and provenance.
    pub request: TurnPrompt,
    /// Owning prepared session.
    pub session_id: SessionId,
}

/// Explicit mutation of an inspected proposal version.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GraphProposalAction {
    /// Regenerate using the captured catalog, model, and remaining call limit.
    Regenerate,
    /// Complete ordered answers to the exact question set.
    Answer(Vec<String>),
    /// Accept the exact inspected version and its captured limits.
    Approve,
    /// Cancel generation or discard a pending proposal permanently.
    Cancel,
}

/// Original immutable host capture, independent of executable graphs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphProposalState {
    /// Captured provider and model for generation and execution.
    #[serde(with = "super::selection")]
    pub agent: AgentSelection,
    /// Captured enabled agent definitions.
    pub definitions: Vec<AgentDefinition>,
    /// Original delivery request.
    pub input: GenerateGraphProposalRequest,
    /// Captured host ceilings; no action replenishes them.
    pub limits: GraphProposalLimits,
    /// Captured permission ceiling.
    #[serde(with = "permission")]
    pub permission: PermissionMode,
    /// Current immutable generation version.
    pub version: GraphProposalVersion,
}

/// A generation reservation becomes terminal once; later work gets a new
/// version.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphProposalVersion {
    /// Complete answer set supplied to this generation, in question order.
    pub answers: Vec<String>,
    /// Approval receipt, bound to this exact outcome and limits.
    pub approval: Option<GraphProposalApproval>,
    /// Terminal discard flag, including cancellation during generation.
    pub is_cancelled: bool,
    /// Consecutive host-assigned generation version.
    pub number: u32,
    /// Generation progress, clarification, failure, or resolved preview.
    pub outcome: GraphProposalVersionOutcome,
    /// Parent reference used to locate the exact generating worker run.
    pub parent_id: String,
    /// Worker-owned utility identity, captured on completion when available.
    pub utility_run_id: Option<String>,
}

/// Generation outcome; only a resolved preview can be approved.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GraphProposalVersionOutcome {
    /// Reserved durably before worker submission; restart never resubmits it.
    Generating,
    /// Complete wire response and optional executable resolution.
    Generated {
        /// Resolved graph and fixed execution budgets, absent for
        /// clarification.
        launch: Option<Box<StartExecutionGraphRequest>>,
        /// Model-authored proposal or clarification.
        response: GraphProposalResponse,
    },
    /// Failed generation or rejected resolution; requires an explicit retry.
    Failed(String),
}

/// Receipt for one exact-version approval, separate from output acceptance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphProposalApproval {
    /// Published executable snapshot identity.
    pub graph_id: String,
    /// Captured ceilings inspected with the approved version.
    pub limits: GraphProposalLimits,
    /// Exact inspected generation version.
    pub version: u32,
}

mod permission {
    use std::borrow::Borrow;

    use serde::{Deserialize, Deserializer, Serializer, de};

    use crate::PermissionMode;

    pub(super) fn serialize<S: Serializer>(
        value: impl Borrow<PermissionMode>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(value.borrow().label())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<PermissionMode, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}
