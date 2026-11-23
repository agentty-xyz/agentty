//! Session API reads of graph proposals and execution graphs, built from the
//! snapshot the terminal inspector loads and with the wording and action
//! rules of the terminal's graph sidebar and inspector, and the session API's
//! snapshot-bound graph actions, which admit the terminal's own mutations.

use std::collections::BTreeSet;

use ag_protocol::{GraphProposalOutcome, GraphProposalResponse};
use ag_session::{
    ExecutionGraph, GraphNodeContinuation, GraphNodeStatus, GraphProposalState,
    GraphProposalVersionOutcome, SessionError, SessionGraph, SessionGraphAction,
    SessionGraphActionKind, SessionGraphActionRequest, SessionGraphEntry, SessionGraphNode,
    SessionGraphOutput, SessionGraphProposal, SessionGraphProposalPhase, SessionGraphRequirement,
    SessionGraphSummary, SessionGraphs, SessionGraphsRequest,
};

use crate::app::App;
use crate::app::command_gateway::SessionMutation;
use crate::app::graph_view::{
    ACTION_UNAVAILABLE, GRAPH_CHANGED, GraphNodeChoice, GraphSelection, GraphSnapshot,
    GraphViewAction, graph_snapshot_token,
};
use crate::domain::agent::ReasoningLevel;
use crate::domain::session::Session;
use crate::presentation::execution_graph::{
    FOCUSED_REVIEW_NOTE, GraphActionTarget, PROPOSAL_REPLY_HINT, QUESTION_REPLY_HINT,
    UNAVAILABLE_STATUS_LABEL, blocked_reason_label, is_focused_review_graph, node_reasoning_level,
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

    /// Builds the mutation a session API graph action admits: the one the
    /// terminal inspector admits for the same action with the same node and
    /// output selected. The graph must belong to the session and still match
    /// the snapshot the action was chosen from, and the action must be one
    /// that snapshot offers.
    pub(super) async fn api_graph_action_mutation(
        &self,
        request: SessionGraphActionRequest,
    ) -> Result<SessionMutation, String> {
        let SessionGraphActionRequest {
            action,
            graph_id,
            session_id,
            snapshot: token,
        } = request;
        // The node, output, and spent budget the action names, if any.
        let (node_id, output_id, budget_id, action) = match action {
            SessionGraphAction::Accept { node_id, output_id } => (
                Some(node_id),
                Some(output_id),
                None,
                Ok(GraphViewAction::Accept),
            ),
            SessionGraphAction::Cancel => (None, None, None, Ok(GraphViewAction::Cancel)),
            SessionGraphAction::ExtendBudget {
                additional_calls,
                budget_id,
            } => (
                None,
                None,
                Some(budget_id),
                GraphViewAction::extend_budget(additional_calls),
            ),
            SessionGraphAction::Reply { message, node_id } => {
                (Some(node_id), None, None, GraphViewAction::reply(&message))
            }
            SessionGraphAction::Resume => (None, None, None, Ok(GraphViewAction::Resume)),
            SessionGraphAction::Retry { node_id } => (
                Some(node_id),
                None,
                None,
                Ok(GraphViewAction::Continue(GraphNodeContinuation::Retry)),
            ),
        };
        let snapshot = self
            .load_graph_snapshot(
                session_id.as_str(),
                GraphSelection {
                    follows_approval: false,
                    graph_id: Some(&graph_id),
                    keeps_proposal: false,
                    node: node_id
                        .as_deref()
                        .map_or(GraphNodeChoice::Index(0), GraphNodeChoice::Id),
                },
            )
            .await?;
        let graph = snapshot
            .graphs
            .get(snapshot.selected_graph)
            .filter(|graph| graph.id == graph_id)
            .ok_or(FOREIGN_GRAPH)?;
        let progress = snapshot.progress.as_ref();
        if graph_snapshot_token(graph, progress) != token {
            return Err(GRAPH_CHANGED.into());
        }
        let action = action?;
        let output = match output_id {
            Some(output_id) => Some(
                snapshot
                    .outputs
                    .iter()
                    .find(|output| output.result.reference.id == output_id)
                    .ok_or(ACTION_UNAVAILABLE)?,
            ),
            None => None,
        };
        let target = GraphActionTarget {
            graph: Some(graph),
            node: snapshot.selected_node,
            output,
            progress,
        };
        // An unknown node selects the first one, which the action must not
        // reach instead.
        let names_target_node = node_id.is_none_or(|node_id| {
            target
                .node_progress()
                .is_some_and(|node| node.node_id == node_id)
        });
        if !names_target_node {
            return Err(ACTION_UNAVAILABLE.into());
        }
        // An extension adds calls to the budget it names, which the
        // terminal offers with a node that waits for it selected.
        let target = match budget_id {
            Some(budget_id) => target
                .with_exhausted_budget(&budget_id)
                .ok_or(ACTION_UNAVAILABLE)?,
            None => target,
        };

        action.mutation(target)
    }
}

/// What the graph of `target` takes as a whole, and the spent budget an
/// extension adds calls to while it takes one.
fn graph_actions(target: GraphActionTarget<'_>) -> (Vec<SessionGraphActionKind>, Option<String>) {
    let can_extend_budget = target.can_extend_budget();
    let actions = offered_actions([
        (SessionGraphActionKind::Cancel, target.can_cancel()),
        (SessionGraphActionKind::ExtendBudget, can_extend_budget),
        (SessionGraphActionKind::Resume, target.can_resume()),
    ]);
    let exhausted_budget = target
        .exhausted_budget()
        .filter(|_| can_extend_budget)
        .map(str::to_string);

    (actions, exhausted_budget)
}

/// What the node of `target` takes.
fn node_actions(target: GraphActionTarget<'_>) -> Vec<SessionGraphActionKind> {
    offered_actions([
        (SessionGraphActionKind::Reply, target.can_continue()),
        (SessionGraphActionKind::Retry, target.can_retry()),
    ])
}

/// The actions whose flag is set, in the given order.
fn offered_actions<const COUNT: usize>(
    actions: [(SessionGraphActionKind, bool); COUNT],
) -> Vec<SessionGraphActionKind> {
    actions
        .into_iter()
        .filter_map(|(action, is_offered)| is_offered.then_some(action))
        .collect()
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
                actions: node_actions(GraphActionTarget {
                    graph: Some(graph),
                    node: index,
                    output: None,
                    progress,
                }),
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
    // Which graph actions apply does not depend on the selected node, but
    // the spent budget does, as the terminal's inspector shows it.
    let (actions, exhausted_budget) = graph_actions(GraphActionTarget {
        graph: Some(graph),
        node: snapshot.selected_node,
        output: None,
        progress,
    });

    SessionGraph {
        actions,
        exhausted_budget,
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
                actions: offered_actions([(
                    SessionGraphActionKind::Accept,
                    GraphActionTarget {
                        graph: Some(graph),
                        node: snapshot.selected_node,
                        output: Some(output),
                        progress,
                    }
                    .can_accept(),
                )]),
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
