//! Durable proposal inspection and explicit action contracts.

use ag_protocol::{GraphProposalResponse, TurnPrompt, TurnPromptAttachment};
use serde::{Deserialize, Serialize};

use super::{ExecutionGraph, GraphProposalLimits};
use crate::{
    AgentDefinition, AgentSelection, PermissionMode, SessionId, StartExecutionGraphRequest,
};

/// Idempotent generation admission; the host captures all other policy,
/// including whether the proposal adds nodes to the session's graph that can
/// still run or proposes a new graph.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GenerateGraphProposalRequest {
    /// Caller delivery identity, reused on retries.
    pub id: String,
    /// Original user request and provenance.
    pub request: TurnPrompt,
    /// Owning prepared session.
    pub session_id: SessionId,
}

impl GenerateGraphProposalRequest {
    /// Asks for a proposal for the task `text`, without attachments, under
    /// the delivery identity `id`.
    #[must_use]
    pub fn from_text(id: String, session_id: SessionId, text: String) -> Self {
        Self {
            id,
            request: TurnPrompt::from_text(text),
            session_id,
        }
    }
}

/// Explicit mutation of an inspected proposal version.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GraphProposalAction {
    /// Regenerate using the captured catalog, model, and remaining call limit.
    Regenerate,
    /// Complete ordered answers to the exact question set.
    Answer(Vec<String>),
    /// Regenerate with free-form user feedback added to the history. Feedback
    /// on a clarification is recorded together with the open questions;
    /// feedback images are kept for every later generation and node.
    Revise(TurnPrompt),
    /// Accept the exact inspected version and its captured limits.
    Approve,
    /// Cancel generation or discard a pending proposal permanently.
    Cancel,
}

/// Original immutable host capture, independent of executable graphs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphProposalState {
    /// Snapshot of the session graph the proposal adds nodes to, at the
    /// revision current when the host admitted the request; `None` for a new
    /// graph. Approval appends the nodes only while that revision is current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<ExecutionGraph>,
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
    /// Revision feedback images, oldest first, delivered after the original
    /// request images.
    #[serde(default)]
    pub attachments: Vec<TurnPromptAttachment>,
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
        /// clarification. For an expansion, the expanded next revision of
        /// the captured base, with no budgets of its own.
        launch: Option<Box<StartExecutionGraphRequest>>,
        /// Model-authored proposal or clarification.
        response: GraphProposalResponse,
    },
    /// Failed generation or rejected resolution; requires an explicit retry.
    Failed(String),
}

/// First decision recorded for a chat reply to one proposal version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphProposalReplyOutcome {
    /// Approve the exact replied-to version and start execution.
    Approve,
    /// Discard the proposal permanently.
    Cancel,
    /// Regenerate with the reply as answers or feedback.
    Revise,
}

/// Receipt for one exact-version approval, separate from output acceptance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphProposalApproval {
    /// Published executable snapshot identity, or the expanded graph's.
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
