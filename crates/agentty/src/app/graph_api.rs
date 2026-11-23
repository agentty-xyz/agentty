//! Session API reads of graph proposals and execution graphs, built from the
//! snapshot the terminal inspector loads and with the wording of the
//! terminal's graph sidebar and inspector.

use std::collections::BTreeSet;

use ag_protocol::{GraphProposalOutcome, GraphProposalResponse};
use ag_session::{
    ExecutionGraph, GraphNodeStatus, GraphProposalState, GraphProposalVersionOutcome, SessionError,
    SessionGraph, SessionGraphEntry, SessionGraphNode, SessionGraphOutput, SessionGraphProposal,
    SessionGraphProposalPhase, SessionGraphRequirement, SessionGraphSummary, SessionGraphs,
    SessionGraphsRequest,
};

use crate::app::App;
use crate::app::graph_view::{
    GraphNodeChoice, GraphSelection, GraphSnapshot, graph_snapshot_token,
};
use crate::domain::agent::ReasoningLevel;
use crate::domain::session::Session;
use crate::presentation::execution_graph::{
    FOCUSED_REVIEW_NOTE, PROPOSAL_REPLY_HINT, QUESTION_REPLY_HINT, UNAVAILABLE_STATUS_LABEL,
    blocked_reason_label, is_focused_review_graph, node_reasoning_level,
    outputs_awaiting_acceptance,
};
use crate::presentation::graph_chat::{
    NodeStatus, edge_label, gate_label, graph_status_label, graph_summary, is_dependency_satisfied,
    proposal_preview, proposal_standing,
};

/// Error for a graph read naming a graph of another session.
const FOREIGN_GRAPH: &str = "Graph does not belong to this session";

impl App {
    /// Summarizes the latest graph proposal of `session_id` and, once it is
    /// approved, its graph's live progress, as the terminal's sidebar does.
    ///
    /// The summary only accompanies a session snapshot, so a failed read
    /// never fails the snapshot: an unreadable proposal leaves no summary,
    /// and unreadable progress leaves the proposal's summary with its status
    /// unavailable, as the sidebar keeps the chat and says so.
    pub(super) async fn api_graph_summary(&self, session_id: &str) -> Option<SessionGraphSummary> {
        let proposal = match self
            .services
            .db()
            .graph_proposals()
            .latest(session_id)
            .await
        {
            Ok(proposal) => proposal?,
            Err(error) => {
                tracing::warn!(session_id, %error, "Graph proposal summary unavailable");

                return None;
            }
        };
        let progress = match &proposal.version.approval {
            Some(approval) => self.execution_graph_progress(&approval.graph_id).await,
            None => Ok(None),
        };

        Some(match progress {
            Ok(progress) => graph_summary(&proposal, progress.as_ref()),
            Err(error) => {
                tracing::warn!(session_id, %error, "Graph progress summary unavailable");

                SessionGraphSummary {
                    label: UNAVAILABLE_STATUS_LABEL.to_string(),
                    ..graph_summary(&proposal, None)
                }
            }
        })
    }

    /// Reads a session's graphs through the terminal inspector's snapshot
    /// load. A named graph must belong to the session; without one, the
    /// latest proposal's approved graph is selected, or else the first.
    pub(super) async fn api_session_graphs(
        &self,
        request: SessionGraphsRequest,
    ) -> Result<SessionGraphs, SessionError> {
        let node = request
            .node_id
            .as_deref()
            .map_or(GraphNodeChoice::Index(0), GraphNodeChoice::Id);
        let snapshot = self
            .load_graph_snapshot(
                request.session_id.as_str(),
                GraphSelection {
                    follows_approval: request.graph_id.is_none(),
                    graph_id: request.graph_id.as_deref(),
                    keeps_proposal: false,
                    node,
                },
            )
            .await
            .map_err(SessionError::Operation)?;
        if let Some(graph_id) = &request.graph_id
            && !snapshot.graphs.iter().any(|graph| graph.id == *graph_id)
        {
            return Err(SessionError::Operation(FOREIGN_GRAPH.to_string()));
        }
        let reasoning_level = self
            .sessions
            .session_for_id(&request.session_id)
            .map_or_default(Session::effective_reasoning_level);
        let graph = snapshot
            .graphs
            .get(snapshot.selected_graph)
            .map(|graph| api_graph(graph, &snapshot, reasoning_level));

        Ok(SessionGraphs {
            graph,
            graphs: snapshot
                .graphs
                .iter()
                .map(|graph| SessionGraphEntry {
                    id: graph.id.clone(),
                    is_focused_review: is_focused_review_graph(graph),
                    revision: graph.revision,
                })
                .collect(),
            proposal: snapshot.proposal.map(api_graph_proposal),
        })
    }
}

/// The selected `graph` of `snapshot` with what the inspector shows about
/// each node; task-conversation nodes run with the session's
/// `reasoning_level`.
fn api_graph(
    graph: &ExecutionGraph,
    snapshot: &GraphSnapshot,
    reasoning_level: ReasoningLevel,
) -> SessionGraph {
    let progress = snapshot.progress.as_ref();
    let awaited = progress.map_or_default(|progress| {
        outputs_awaiting_acceptance(progress)
            .into_iter()
            .map(|(producer, _)| producer.node_id.clone())
            .collect::<BTreeSet<_>>()
    });
    let nodes = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            let state = progress
                .and_then(|progress| progress.nodes.iter().find(|state| state.node_id == node.id));
            let awaits_approval = awaited.contains(&node.id);
            let live_status = state.map_or(GraphNodeStatus::Pending, |state| state.status);
            let status = if awaits_approval {
                NodeStatus::AwaitingApproval
            } else {
                NodeStatus::Live(live_status)
            };
            let previous = index
                .checked_sub(1)
                .map(|previous| graph.nodes[previous].id.as_str());

            SessionGraphNode {
                awaits_approval,
                blocked: state.map_or_default(|state| {
                    state.blocked.iter().map(blocked_reason_label).collect()
                }),
                hint: (live_status == GraphNodeStatus::AwaitingAnswers)
                    .then_some(QUESTION_REPLY_HINT.to_string()),
                node: node.clone(),
                progress: state.cloned(),
                reasoning_level: node_reasoning_level(node, reasoning_level),
                requirements: node
                    .dependencies
                    .iter()
                    .map(|dependency| SessionGraphRequirement {
                        gate: gate_label(&dependency.gate),
                        is_satisfied: is_dependency_satisfied(progress, node, dependency),
                        label: edge_label(dependency, previous),
                        node_id: dependency.node_id.clone(),
                    })
                    .collect(),
                status_label: status.label().to_string(),
            }
        })
        .collect();
    let is_focused_review = is_focused_review_graph(graph);

    SessionGraph {
        id: graph.id.clone(),
        is_focused_review,
        node_id: progress
            .and_then(|progress| progress.nodes.get(snapshot.selected_node))
            .map(|node| node.node_id.clone()),
        nodes,
        note: is_focused_review.then_some(FOCUSED_REVIEW_NOTE.to_string()),
        outputs: snapshot
            .outputs
            .iter()
            .map(|output| SessionGraphOutput {
                body: output.body.clone(),
                display: output.display().to_string(),
                result: output.result.clone(),
            })
            .collect(),
        revision: graph.revision,
        snapshot: graph_snapshot_token(graph, progress),
        status: progress.map(|progress| progress.status),
        status_label: progress
            .map_or(UNAVAILABLE_STATUS_LABEL, |progress| {
                graph_status_label(progress.status)
            })
            .to_string(),
    }
}

/// The latest proposal with its preview and, while it waits for the user,
/// how to act on it.
fn api_graph_proposal(state: GraphProposalState) -> SessionGraphProposal {
    let (phase, _) = proposal_standing(&state);
    let is_pending = !matches!(
        phase,
        SessionGraphProposalPhase::Approved | SessionGraphProposalPhase::Cancelled
    );
    let questions = match &state.version.outcome {
        GraphProposalVersionOutcome::Generated {
            response:
                GraphProposalResponse {
                    outcome: GraphProposalOutcome::Clarification { questions },
                },
            ..
        } => questions.clone(),
        _ => Vec::new(),
    };

    SessionGraphProposal {
        hint: is_pending.then_some(PROPOSAL_REPLY_HINT.to_string()),
        phase,
        preview: proposal_preview(&state),
        questions,
        state,
    }
}
