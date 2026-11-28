//! Graph inspector reads and snapshot-bound mutations through shared admission.

use std::hash::{DefaultHasher, Hash, Hasher};

use ag_session::{
    ContinueGraphNodeRequest, ExecutionGraph, ExecutionGraphProgress, ExtendGraphBudgetRequest,
    GraphControlRequest, GraphNode, GraphNodeContinuation, GraphProposalState,
    ResumeExecutionGraphRequest,
};
use uuid::Uuid;

use super::App;
use super::command_gateway::{CommandDelivery, CommandInitiator, SessionCommand, SessionMutation};
use crate::presentation::app_mode::{AppMode, ConfirmationViewMode};
use crate::presentation::execution_graph::{
    GraphActionTarget, GraphEditKind, GraphView, GraphViewOutput,
};

impl App {
    pub(crate) async fn open_graph_view(&mut self, restore: ConfirmationViewMode) {
        let mut view = GraphView::new(restore);
        if let Err(error) = self.load_graph_view(&mut view).await {
            view.notice = error;
        }
        self.mode = AppMode::ExecutionGraph(Box::new(view));
    }

    /// Refresh is explicit so an action always targets what the user inspected.
    pub(crate) async fn load_graph_view(&self, view: &mut GraphView) -> Result<(), String> {
        let graph_id = view.graph().map(|graph| graph.id.clone());
        let snapshot = self
            .load_graph_snapshot(
                &view.restore.session_id,
                GraphSelection {
                    follows_approval: view.is_proposal_selected,
                    graph_id: graph_id.as_deref(),
                    keeps_proposal: view.is_proposal_selected,
                    node: if view.selects_first_node {
                        GraphNodeChoice::First
                    } else {
                        GraphNodeChoice::Index(view.selected_node)
                    },
                },
            )
            .await?;
        view.selects_first_node = false;
        view.selected_output = view
            .selected_output
            .min(snapshot.outputs.len().saturating_sub(1));
        view.selected_graph = snapshot.selected_graph;
        view.selected_node = snapshot.selected_node;
        view.graphs = snapshot.graphs;
        view.is_proposal_selected = snapshot.is_proposal_selected;
        view.proposal = snapshot.proposal;
        view.progress = snapshot.progress;
        view.outputs = snapshot.outputs;

        Ok(())
    }

    /// Reads a session's graphs, its latest proposal, and the selected
    /// graph's live progress and selected-node outputs in one pass, which
    /// fails as a whole. The terminal inspector and the session API share
    /// it, so both show the same snapshot.
    pub(crate) async fn load_graph_snapshot(
        &self,
        session_id: &str,
        selection: GraphSelection<'_>,
    ) -> Result<GraphSnapshot, String> {
        let graphs = self.services.db().graphs();
        let mut selected_id = selection.graph_id.map(str::to_string);
        let proposal = self
            .services
            .db()
            .graph_proposals()
            .latest(session_id)
            .await
            .map_err(|error| error.to_string())?;
        let is_proposal_selected = selection.keeps_proposal
            && proposal
                .as_ref()
                .is_some_and(|state| state.version.approval.is_none());
        if selection.follows_approval
            && let Some(approval) = proposal
                .as_ref()
                .and_then(|state| state.version.approval.as_ref())
        {
            selected_id = Some(approval.graph_id.clone());
        }
        let definitions = graphs
            .list_graphs(session_id)
            .await
            .map_err(|error| error.to_string())?;
        let selected_graph = selected_id
            .and_then(|id| definitions.iter().position(|graph| graph.id == id))
            .unwrap_or(0);
        let progress = match definitions
            .get(selected_graph)
            .filter(|_| !is_proposal_selected)
        {
            Some(graph) => self
                .execution_graph_progress(&graph.id)
                .await
                .map_err(|error| error.to_string())?,
            None => None,
        };
        let selected_node = selection.node.index(
            definitions
                .get(selected_graph)
                .filter(|_| !is_proposal_selected),
            progress.as_ref(),
        );
        let mut outputs = Vec::new();
        if let Some(node) = progress
            .as_ref()
            .and_then(|progress| progress.nodes.get(selected_node))
        {
            for result in &node.results {
                if let Some(stored) = graphs
                    .load_result(&result.reference.id)
                    .await
                    .map_err(|error| error.to_string())?
                {
                    outputs.push(GraphViewOutput::new(result.clone(), stored.body));
                }
            }
        }

        Ok(GraphSnapshot {
            graphs: definitions,
            is_proposal_selected,
            outputs,
            progress,
            proposal,
            selected_graph,
            selected_node,
        })
    }

    /// Refreshes a rejected draft only while its original target remains
    /// actionable.
    pub(crate) async fn reconcile_graph_editor(&self, view: &mut GraphView) -> Result<(), String> {
        let mut refreshed = view.clone();
        self.load_graph_view(&mut refreshed).await?;
        if !view.can_reconcile_editor(&refreshed) {
            return Err(
                "The original target changed or no longer accepts this action. Draft retained; \
                 Esc discards it."
                    .into(),
            );
        }
        refreshed
            .editor
            .as_mut()
            .ok_or("No graph editor")?
            .needs_reconciliation = false;
        *view = refreshed;

        Ok(())
    }

    /// Reject a superseded graph/execution/output before admitting an action.
    pub(crate) async fn apply_graph_view_action(
        &mut self,
        view: &GraphView,
        action: GraphViewAction,
    ) -> Result<(), String> {
        let graph = view.graph().ok_or("No graph selected")?;
        let current = self
            .execution_graph_progress(&graph.id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("Graph no longer exists")?;
        let snapshot = view.progress.as_ref().ok_or("No graph progress")?;
        if snapshot.graph_id != graph.id
            || snapshot.revision != graph.revision
            || current.session_id != graph.session_id
            || current.revision != graph.revision
            || current.nodes != snapshot.nodes
            || current.status != snapshot.status
        {
            return Err(GRAPH_CHANGED.into());
        }
        let request = action.mutation(view.action_target())?;
        self.admit_session_command(SessionCommand::new(
            CommandInitiator::SessionView,
            CommandDelivery::Admission {
                request,
                response_tx: None,
            },
        ))
        .await
        .map_err(|error| error.to_string())
    }

    pub(crate) fn graph_editor_action(view: &GraphView) -> Result<GraphViewAction, String> {
        let editor = view.editor.as_ref().ok_or("No graph editor")?;
        let text = editor.input.text().trim();
        match editor.kind {
            GraphEditKind::Cancel if text == "cancel" => Ok(GraphViewAction::Cancel),
            GraphEditKind::Cancel => Err("Type cancel to confirm graph cancellation".into()),
            GraphEditKind::ExtendBudget => text
                .parse::<usize>()
                .map_err(|_| GraphViewAction::budget_range_error())
                .and_then(GraphViewAction::extend_budget),
            GraphEditKind::Reply => GraphViewAction::reply(text),
        }
    }
}

/// Refusal of an action chosen from a graph snapshot that no longer holds.
pub(crate) const GRAPH_CHANGED: &str =
    "Graph changed; refresh the view and inspect it before acting again.";

/// Refusal of an action its target does not take.
pub(crate) const ACTION_UNAVAILABLE: &str = "Action unavailable for this snapshot";

pub(crate) enum GraphViewAction {
    Accept,
    Cancel,
    Continue(GraphNodeContinuation),
    /// Adds this many provider calls to the exhausted budget.
    ExtendBudget(usize),
    Resume,
}

impl GraphViewAction {
    /// Adds `additional_calls` provider calls, refusing a number outside
    /// `1..=`[`ExtendGraphBudgetRequest::MAX_ADDITIONAL_CALLS`].
    pub(crate) fn extend_budget(additional_calls: usize) -> Result<Self, String> {
        if (1..=ExtendGraphBudgetRequest::MAX_ADDITIONAL_CALLS).contains(&additional_calls) {
            return Ok(Self::ExtendBudget(additional_calls));
        }

        Err(Self::budget_range_error())
    }

    /// Replies with `text` as revision guidance, refusing a blank reply.
    pub(crate) fn reply(text: &str) -> Result<Self, String> {
        let message = text.trim();
        if message.is_empty() {
            return Err("Enter a reply".into());
        }

        Ok(Self::Continue(GraphNodeContinuation::Reply {
            attachments: Vec::new(),
            message: message.to_string(),
        }))
    }

    /// Builds the mutation that applies this action to `target`, refusing
    /// an action `target` does not take. Every frontend admits the same
    /// mutation for the same action.
    pub(crate) fn mutation(self, target: GraphActionTarget<'_>) -> Result<SessionMutation, String> {
        let graph = target.graph.ok_or("No graph selected")?;
        let control = GraphControlRequest {
            graph_id: graph.id.clone(),
            revision: graph.revision,
        };

        Ok(match self {
            Self::Accept if let Some(output) = target.output.filter(|_| target.can_accept()) => {
                SessionMutation::AcceptExecutionGraphResult {
                    reference: Box::new(output.result.reference.clone()),
                }
            }
            Self::Cancel if target.can_cancel() => {
                SessionMutation::CancelExecutionGraph { request: control }
            }
            Self::Resume if target.can_resume() => SessionMutation::ResumeExecutionGraph {
                request: ResumeExecutionGraphRequest { graph: control },
            },
            Self::ExtendBudget(additional_calls)
                if let Some(budget_id) = target
                    .exhausted_budget()
                    .filter(|_| target.can_extend_budget()) =>
            {
                SessionMutation::ExtendExecutionGraphBudget {
                    request: ExtendGraphBudgetRequest {
                        action_id: Uuid::new_v4().to_string(),
                        additional_calls,
                        budget_id: budget_id.to_string(),
                        graph: control,
                    },
                }
            }
            Self::Continue(kind)
                if match &kind {
                    GraphNodeContinuation::Retry => target.can_retry(),
                    _ => target.can_continue(),
                } =>
            {
                let node = target.node_progress().ok_or("No node selected")?;
                SessionMutation::ContinueExecutionGraphNode {
                    request: Box::new(ContinueGraphNodeRequest {
                        action_id: Uuid::new_v4().to_string(),
                        execution: node
                            .executions
                            .last()
                            .ok_or("No execution selected")?
                            .clone(),
                        graph_id: graph.id.clone(),
                        kind,
                        node_id: node.node_id.clone(),
                        revision: graph.revision,
                    }),
                }
            }
            _ => return Err(ACTION_UNAVAILABLE.into()),
        })
    }

    fn budget_range_error() -> String {
        format!(
            "Enter a whole number of provider calls from 1 to {}",
            ExtendGraphBudgetRequest::MAX_ADDITIONAL_CALLS
        )
    }
}

/// Which graph and node [`App::load_graph_snapshot`] selects.
pub(crate) struct GraphSelection<'a> {
    /// Selects the latest proposal's approved graph instead of `graph_id`
    /// once that proposal is approved.
    pub(crate) follows_approval: bool,
    /// Graph to select; a graph the session does not have, or `None`,
    /// selects its first graph.
    pub(crate) graph_id: Option<&'a str>,
    /// Keeps the latest proposal selected while it is unapproved, without
    /// reading graph progress.
    pub(crate) keeps_proposal: bool,
    /// Node of the selected graph to select.
    pub(crate) node: GraphNodeChoice<'a>,
}

/// A node chosen by declaration position, by identity, or as the first row
/// of the inspector's list.
pub(crate) enum GraphNodeChoice<'a> {
    /// The node the inspector lists first: the first top-level node, even
    /// when a child is declared before it.
    First,
    /// The node at this position, or the last one when the graph has fewer.
    Index(usize),
    /// The node with this identity, or the first one when the graph has
    /// none.
    Id(&'a str),
}

impl GraphNodeChoice<'_> {
    /// Returns the position of the chosen node of `graph` in `progress`, or
    /// zero when there is no progress.
    fn index(
        &self,
        graph: Option<&ExecutionGraph>,
        progress: Option<&ExecutionGraphProgress>,
    ) -> usize {
        let nodes = progress.map_or_default(|progress| progress.nodes.as_slice());
        match self {
            Self::First => graph
                .and_then(|graph| GraphNode::hierarchy_order(&graph.nodes).first().copied())
                .map_or(0, |placement| placement.index)
                .min(nodes.len().saturating_sub(1)),
            Self::Index(index) => (*index).min(nodes.len().saturating_sub(1)),
            Self::Id(node_id) => nodes
                .iter()
                .position(|node| node.node_id == *node_id)
                .unwrap_or(0),
        }
    }
}

/// One consistent read of a session's graphs from
/// [`App::load_graph_snapshot`].
pub(crate) struct GraphSnapshot {
    /// The session's graph definitions in creation order.
    pub(crate) graphs: Vec<ExecutionGraph>,
    /// Whether the unapproved latest proposal stays selected.
    pub(crate) is_proposal_selected: bool,
    /// Outputs of the selected node with their stored bodies.
    pub(crate) outputs: Vec<GraphViewOutput>,
    /// Live progress of the selected graph, unless the proposal is selected.
    pub(crate) progress: Option<ExecutionGraphProgress>,
    /// The session's latest proposal.
    pub(crate) proposal: Option<GraphProposalState>,
    /// Position of the selected graph in `graphs`.
    pub(crate) selected_graph: usize,
    /// Position of the selected node in `progress`.
    pub(crate) selected_node: usize,
}

/// Opaque token of what an action on `graph` is checked against, as
/// [`App::apply_graph_view_action`] compares it: the graph's identity,
/// session, and revision, and its live status and node progress. It is
/// stable within one process.
pub(crate) fn graph_snapshot_token(
    graph: &ExecutionGraph,
    progress: Option<&ExecutionGraphProgress>,
) -> String {
    let mut hasher = DefaultHasher::new();
    graph.id.hash(&mut hasher);
    graph.session_id.as_str().hash(&mut hasher);
    graph.revision.hash(&mut hasher);
    format!(
        "{:?}",
        progress.map(|progress| (progress.status, &progress.nodes))
    )
    .hash(&mut hasher);

    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
#[path = "graph_view_test.rs"]
mod tests;
