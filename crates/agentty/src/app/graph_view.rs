//! Graph inspector reads and snapshot-bound mutations through shared admission.

use ag_session::{
    ContinueGraphNodeRequest, GraphControlRequest, GraphNodeContinuation,
    ResumeExecutionGraphRequest,
};
use uuid::Uuid;

use super::App;
use super::command_gateway::{CommandDelivery, CommandInitiator, SessionCommand, SessionMutation};
use crate::presentation::app_mode::{AppMode, ConfirmationViewMode};
use crate::presentation::execution_graph::{GraphEditKind, GraphView, GraphViewOutput};

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
        let graphs = self.services.db().graphs();
        let mut selected_id = view.graph().map(|graph| graph.id.clone());
        let proposal = if self.experimental_graph {
            self.services
                .db()
                .graph_proposals()
                .latest(&view.restore.session_id)
                .await
                .map_err(|error| error.to_string())?
        } else {
            None
        };
        let is_proposal_selected = view.is_proposal_selected
            && proposal
                .as_ref()
                .is_some_and(|state| state.version.approval.is_none());
        if view.is_proposal_selected
            && let Some(approval) = proposal
                .as_ref()
                .and_then(|state| state.version.approval.as_ref())
        {
            selected_id = Some(approval.graph_id.clone());
        }
        let definitions = graphs
            .list_graphs(&view.restore.session_id)
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
        let selected_node = view.selected_node.min(
            progress
                .as_ref()
                .map_or(0, |progress| progress.nodes.len().saturating_sub(1)),
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
        view.selected_output = view.selected_output.min(outputs.len().saturating_sub(1));
        view.selected_graph = selected_graph;
        view.selected_node = selected_node;
        view.graphs = definitions;
        view.is_proposal_selected = is_proposal_selected;
        view.proposal = proposal;
        view.progress = progress;
        view.outputs = outputs;

        Ok(())
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
            return Err(
                "Graph changed; refresh the view and inspect it before acting again.".into(),
            );
        }
        let control = GraphControlRequest {
            graph_id: graph.id.clone(),
            revision: graph.revision,
        };
        let request = match action {
            GraphViewAction::Accept if view.can_accept() => {
                SessionMutation::AcceptExecutionGraphResult {
                    reference: Box::new(
                        view.outputs[view.selected_output].result.reference.clone(),
                    ),
                }
            }
            GraphViewAction::Cancel if view.can_cancel() => {
                SessionMutation::CancelExecutionGraph { request: control }
            }
            GraphViewAction::Resume if view.can_resume() => SessionMutation::ResumeExecutionGraph {
                request: ResumeExecutionGraphRequest { graph: control },
            },
            GraphViewAction::Continue(kind)
                if match &kind {
                    GraphNodeContinuation::Retry => view.can_retry(),
                    _ => view.can_continue(),
                } =>
            {
                let node = view.node().ok_or("No node selected")?;
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
            _ => return Err("Action unavailable for this snapshot".into()),
        };
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
            GraphEditKind::Reply if !text.is_empty() => {
                Ok(GraphViewAction::Continue(GraphNodeContinuation::Reply {
                    attachments: Vec::new(),
                    message: text.to_string(),
                }))
            }
            GraphEditKind::Reply => Err("Enter a reply".into()),
        }
    }
}

pub(crate) enum GraphViewAction {
    Accept,
    Cancel,
    Continue(GraphNodeContinuation),
    Resume,
}

#[cfg(test)]
#[path = "graph_view_test.rs"]
mod tests;
