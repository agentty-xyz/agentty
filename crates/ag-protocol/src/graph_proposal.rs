//! Model-authored graph proposals, before host resolution and approval.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ProtocolRequestProfile, QuestionItem};

/// Direct structured response to an initial graph-generation request.
///
/// The object wrapper keeps the transport schema rooted at an object. A
/// proposal describes requirements only: it grants no execution authority.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphProposalResponse {
    /// Either missing information or one complete proposal, never both.
    pub outcome: GraphProposalOutcome,
}

/// Mutually exclusive outcomes of proposal generation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GraphProposalOutcome {
    /// Ask only for information required to propose a complete graph.
    Clarification {
        /// Nonempty ordered questions; no partial graph accompanies them.
        #[schemars(length(min = 1, max = crate::model::MAX_QUESTIONS))]
        questions: Vec<QuestionItem>,
    },
    /// Supply the complete graph for host validation and inspection.
    Proposal {
        /// Declarative graph; resolution and approval remain host-owned.
        proposal: GraphProposal,
    },
}

/// Complete logical graph, without runtime identities or acceptance evidence.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphProposal {
    /// Nonempty nodes whose references are local to this proposal.
    #[schemars(length(min = 1))]
    pub nodes: Vec<GraphProposalNode>,
}

/// One proposed invocation of an available agent definition.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphProposalNode {
    /// Exact identifier from the host-supplied available agent catalog.
    pub agent_id: String,
    /// All prerequisites required before this node may run.
    pub dependencies: Vec<GraphProposalDependency>,
    /// Declarative requirements, subject to host support and permissions.
    pub execution: GraphProposalExecution,
    /// Logical identifier unique within this proposal, not a runtime ID.
    pub id: String,
    /// Named handoffs from declared outputs on prerequisite nodes.
    pub inputs: Vec<GraphProposalInput>,
    /// Desired outcome, separate from the referenced agent's instructions.
    pub objective: String,
    /// Named outputs containing the complete response under its schema.
    pub outputs: Vec<GraphProposalOutput>,
}

/// Execution requirements that contain no model, Env, task, or budget IDs.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphProposalExecution {
    /// Fresh utility or isolated task conversation; the host allocates context.
    pub context: GraphProposalContext,
    /// Requested host effects; this does not authorize those effects.
    pub post_processing: GraphProposalPostProcessing,
    /// Protocol schema to enforce for this node's response.
    pub response_profile: ProtocolRequestProfile,
    /// Requested Env access, intersected with host and agent restrictions.
    pub workspace_access: GraphProposalWorkspaceAccess,
}

/// Context lifetime requested without a host-owned task identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphProposalContext {
    /// Isolated utility with a fresh context for each execution.
    Fresh,
    /// Isolated, resumable task context allocated by the host.
    TaskConversation,
}

/// Requested host post-processing, independent of model tools.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphProposalPostProcessing {
    /// No mutating host effects.
    None,
    /// Ordered session effects, if authorized by the host.
    SessionWorkflow,
}

/// Resource requirements within the session Env, never a permission grant.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphProposalWorkspaceAccess {
    /// Share read-only access with other graph readers.
    SharedRead,
    /// Fresh read-only utility that tolerates concurrent ordinary host work.
    BackgroundRead,
    /// Serialize against all other Env work; writes still require permission.
    ExclusiveWrite,
}

/// Logical prerequisite; evidence satisfying it must come from the host.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphProposalDependency {
    /// Required condition, distinct from approval of the graph itself.
    pub gate: GraphProposalGate,
    /// Producing node's logical identifier in this proposal.
    pub node_id: String,
}

/// Host-established condition needed before a dependent node runs.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GraphProposalGate {
    /// Successful execution including ordered host effects and no questions.
    ExecutionSucceeded {},
    /// Successful, question-free execution with a validated named output.
    ResultValidated {
        /// Declared output on the prerequisite node.
        output_name: String,
    },
    /// Explicit host-recorded acceptance of an exact validated output version.
    ResultAccepted {
        /// Declared output on the prerequisite node.
        output_name: String,
    },
}

/// Named input referencing a producer's output, without an execution identity.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphProposalInput {
    /// Slot name unique on the consumer.
    pub name: String,
    /// Declared output name on the producer.
    pub output_name: String,
    /// Logical producer node, protected by a matching result dependency gate.
    pub producer_node_id: String,
}

/// Named complete response; it does not assert validation or acceptance.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphProposalOutput {
    /// Output name unique on the producer.
    pub name: String,
    /// Must match the producing node's response profile.
    pub schema: ProtocolRequestProfile,
}
