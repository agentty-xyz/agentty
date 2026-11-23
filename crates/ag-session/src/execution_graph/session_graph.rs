//! Read models a frontend shows for a session's graph proposals and execution
//! graphs: a compact summary beside the chat and an inspector snapshot with
//! the actions it offers, plus the snapshot-bound action request. Hosts fill
//! every label and offered action with the wording and rules their own views
//! use, so frontends show them without restating them.

use std::num::NonZeroU64;

use super::{ExecutionGraphStatus, GraphNode, GraphNodeProgress, GraphProposalState, GraphResult};
use crate::{QuestionItem, ReasoningLevel, SessionId};

/// What a frontend shows beside a session's chat about its latest graph
/// proposal and, once approved, the graph that proposal launched.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionGraphSummary {
    /// Nodes whose validated output a dependent waits for the user to
    /// accept, in declaration order.
    pub awaiting_approval: Vec<SessionGraphApproval>,
    /// Nodes of the launched graph that succeeded.
    pub done: usize,
    /// Line describing `phase`, or the launched graph's status, as the host
    /// shows it.
    pub label: String,
    /// Where the latest proposal stands.
    pub phase: SessionGraphProposalPhase,
    /// Live status of the approved graph, once the host reads it.
    pub status: Option<ExecutionGraphStatus>,
    /// Nodes of the resolved proposal; zero before one resolves and after
    /// cancellation.
    pub total: usize,
    /// Number of the latest proposal version.
    pub version: u32,
}

/// One node output that waits for the user's approval.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionGraphApproval {
    /// Node that produced the output.
    pub node_id: String,
    /// Whether a chat reply may also revise the output instead of accepting
    /// it.
    pub takes_revision: bool,
    /// Line telling the user how to act on it, as the host shows it.
    pub text: String,
}

/// Where a graph proposal version stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionGraphProposalPhase {
    /// The user approved this version and its graph launched.
    Approved,
    /// The version resolved to a graph that waits for the user's approval.
    AwaitingApproval,
    /// The proposal was discarded; no graph launched.
    Cancelled,
    /// Generation failed; a reply can revise or cancel it.
    Failed,
    /// The model is still generating this version.
    Generating,
    /// The model asked clarification questions first.
    NeedsInput,
    /// The model proposed a graph the host could not resolve.
    Unresolved,
}

/// Which graph a [`SessionGraphs`] read selects, and which of its nodes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionGraphsRequest {
    /// Graph to select; it must belong to `session_id`. `None` selects the
    /// latest proposal's approved graph, or else the session's first graph.
    pub graph_id: Option<String>,
    /// Node of the selected graph to select; `None`, or a node the graph no
    /// longer has, selects its first node.
    pub node_id: Option<String>,
    /// Session whose graphs to read.
    pub session_id: SessionId,
}

/// A session's graphs as an inspector shows them, without any action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionGraphs {
    /// The selected graph, or `None` when the session has none.
    pub graph: Option<SessionGraph>,
    /// Every graph of the session in creation order.
    pub graphs: Vec<SessionGraphEntry>,
    /// The session's latest graph proposal, if any.
    pub proposal: Option<SessionGraphProposal>,
}

/// One graph of a session in a [`SessionGraphs`] listing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionGraphEntry {
    /// Graph identity.
    pub id: String,
    /// Whether the graph is a focused review that the host drives.
    pub is_focused_review: bool,
    /// Current immutable revision.
    pub revision: NonZeroU64,
}

/// A session's latest graph proposal with its rendered preview.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionGraphProposal {
    /// How the user acts on the proposal while it is pending, as the host
    /// says it; `None` once it is approved or cancelled.
    pub hint: Option<String>,
    /// Where the proposal stands.
    pub phase: SessionGraphProposalPhase,
    /// The proposal's details as Markdown, with its captured request and
    /// node instructions quoted.
    pub preview: String,
    /// Clarification questions the version asks before it can resolve.
    pub questions: Vec<QuestionItem>,
    /// The retained proposal state.
    pub state: GraphProposalState,
}

/// The selected graph of a [`SessionGraphs`] read with its live progress.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionGraph {
    /// Actions the graph takes as a whole now, among
    /// [`SessionGraphActionKind::Cancel`],
    /// [`SessionGraphActionKind::ExtendBudget`], and
    /// [`SessionGraphActionKind::Resume`], as the host's inspector offers
    /// them.
    pub actions: Vec<SessionGraphActionKind>,
    /// The spent budget [`SessionGraphActionKind::ExtendBudget`] adds
    /// provider calls to, while `actions` offers it: the selected node's,
    /// or else the first one a pending node waits for, as the host's
    /// inspector shows it.
    pub exhausted_budget: Option<String>,
    /// Graph identity.
    pub id: String,
    /// Whether the graph is a focused review that the host drives.
    pub is_focused_review: bool,
    /// The selected node, or `None` when the graph has no progress yet.
    pub node_id: Option<String>,
    /// Every node in declaration order.
    pub nodes: Vec<SessionGraphNode>,
    /// What the host says about how the graph is driven, such as for a
    /// focused review, when it says anything.
    pub note: Option<String>,
    /// Outputs of the selected node's latest execution.
    pub outputs: Vec<SessionGraphOutput>,
    /// Current immutable revision.
    pub revision: NonZeroU64,
    /// Opaque token of everything an action on this graph is checked
    /// against: identity, revision, status, and node progress. It changes
    /// whenever any of them does, so an action can name the exact snapshot
    /// it was chosen from.
    pub snapshot: String,
    /// Live status, or `None` when the host has no progress for the graph.
    pub status: Option<ExecutionGraphStatus>,
    /// `status` as the host shows it.
    pub status_label: String,
}

/// One node of a [`SessionGraph`] with what a frontend shows about it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionGraphNode {
    /// Actions the node takes now, among [`SessionGraphActionKind::Reply`]
    /// and [`SessionGraphActionKind::Retry`], as the host's inspector offers
    /// them with the node selected.
    pub actions: Vec<SessionGraphActionKind>,
    /// Whether a dependent waits for the user to accept this node's output.
    pub awaits_approval: bool,
    /// Why the pending node cannot dispatch, each as the host shows it.
    pub blocked: Vec<String>,
    /// How the user acts on the node, such as answering its questions in
    /// chat, when it waits for them.
    pub hint: Option<String>,
    /// The node's definition.
    pub node: GraphNode,
    /// The node's live progress, once the host reads it.
    pub progress: Option<GraphNodeProgress>,
    /// Reasoning effort the node runs with.
    pub reasoning_level: ReasoningLevel,
    /// The node's dependencies in declaration order.
    pub requirements: Vec<SessionGraphRequirement>,
    /// The node's status as the host shows it.
    pub status_label: String,
}

/// One dependency of a [`SessionGraphNode`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionGraphRequirement {
    /// What the dependency waits for, as the host shows it.
    pub gate: String,
    /// Whether the dependency no longer blocks the node.
    pub is_satisfied: bool,
    /// The dependency as a node timeline shows it, naming its source.
    pub label: String,
    /// Node the dependency waits for.
    pub node_id: String,
}

/// One output of the selected node with its stored body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionGraphOutput {
    /// Actions the output takes now, [`SessionGraphActionKind::Accept`] or
    /// none, as the host's inspector offers them with the output selected.
    pub actions: Vec<SessionGraphActionKind>,
    /// The stored output body, unchanged.
    pub body: String,
    /// The body as Markdown a frontend shows.
    pub display: String,
    /// The output's exact reference and evidence state.
    pub result: GraphResult,
}

/// An action a graph inspector offers on a graph, one of its nodes, or one
/// of the selected node's outputs.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SessionGraphActionKind {
    /// Accept a validated output, releasing the dependents that wait for it.
    Accept,
    /// Cancel the graph for good.
    Cancel,
    /// Add provider calls to the graph's spent budget.
    ExtendBudget,
    /// Send guidance that revises a settled node's output.
    Reply,
    /// Restart dispatch of a paused graph.
    Resume,
    /// Run a failed or cancelled node again.
    Retry,
}

/// One inspector action on a session's graph, bound to the snapshot it was
/// chosen from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionGraphActionRequest {
    /// What to do.
    pub action: SessionGraphAction,
    /// Graph to act on; it must belong to `session_id`.
    pub graph_id: String,
    /// Session that owns the graph.
    pub session_id: SessionId,
    /// [`SessionGraph::snapshot`] of the read the action was chosen from.
    /// The host refuses the action once the graph no longer matches it.
    pub snapshot: String,
}

/// What a [`SessionGraphActionRequest`] does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionGraphAction {
    /// Accepts `output_id`, one output of `node_id`'s latest execution.
    Accept {
        /// Node that produced the output.
        node_id: String,
        /// [`crate::GraphResultRef::id`] of the output.
        output_id: String,
    },
    /// Cancels the graph permanently, without cancelling other session work.
    Cancel,
    /// Adds provider calls to `budget_id`, a spent budget of the graph.
    ExtendBudget {
        /// Calls to add, from one to
        /// [`crate::ExtendGraphBudgetRequest::MAX_ADDITIONAL_CALLS`].
        additional_calls: usize,
        /// The spent budget to extend, as [`SessionGraph::exhausted_budget`]
        /// named it; a pending node of the snapshot must wait for it.
        budget_id: String,
    },
    /// Sends `message` as guidance that revises `node_id`'s output.
    Reply {
        /// Guidance text; it must not be blank.
        message: String,
        /// Node to revise.
        node_id: String,
    },
    /// Restarts dispatch of a paused graph.
    Resume,
    /// Runs `node_id` again after it failed or was cancelled.
    Retry {
        /// Node to run again.
        node_id: String,
    },
}
