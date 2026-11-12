//! Snapshot-bound state for the generic graph inspector.

use std::collections::{BTreeMap, BTreeSet};

use ag_protocol::{ProtocolRequestProfile, parse_protocol_response_strict};
use ag_session::{
    ExecutionGraph, ExecutionGraphProgress, ExecutionGraphStatus, GraphBlockedReason,
    GraphCompletionGate, GraphContext, GraphNode, GraphNodeProgress, GraphNodeStatus,
    GraphProposalState, GraphResult, GraphResultState, GraphWorkspaceAccess,
};

use super::app_mode::ConfirmationViewMode;
use crate::domain::input::InputState;

/// A session's graph inspector. Editing retains the snapshot that owns the
/// action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphView {
    /// Current editor, if any.
    pub(crate) editor: Option<GraphEditor>,
    /// Current persisted graph definitions in creation order.
    pub(crate) graphs: Vec<ExecutionGraph>,
    /// Selects the proposal instead of a launched execution graph.
    pub(crate) is_proposal_selected: bool,
    /// Most recent action or refresh diagnostic.
    pub(crate) notice: String,
    /// Vertical offset in the editor's wrapped diagnostic.
    pub(crate) notice_scroll: u16,
    /// Exact results and display text derived once per snapshot load.
    pub(crate) outputs: Vec<GraphViewOutput>,
    /// Latest authoritative projection for the selected graph.
    pub(crate) progress: Option<ExecutionGraphProgress>,
    /// Latest retained proposal, shown read-only; chat replies act on it.
    pub(crate) proposal: Option<GraphProposalState>,
    /// Original live limits, or empty after restart; refresh replaces this
    /// hint.
    pub(crate) resume_limits: String,
    /// Session view restored on exit.
    pub(crate) restore: ConfirmationViewMode,
    /// Vertical offset in node details and output.
    pub(crate) scroll: u16,
    /// Selected graph in creation order.
    pub(crate) selected_graph: usize,
    /// Selected node in declaration order.
    pub(crate) selected_node: usize,
    /// Selected output for inspection and acceptance.
    pub(crate) selected_output: usize,
}

impl GraphView {
    pub(crate) fn new(restore: ConfirmationViewMode) -> Self {
        Self {
            editor: None,
            graphs: Vec::new(),
            is_proposal_selected: true,
            notice: String::new(),
            notice_scroll: 0,
            outputs: Vec::new(),
            progress: None,
            proposal: None,
            resume_limits: String::new(),
            restore,
            scroll: 0,
            selected_graph: 0,
            selected_node: 0,
            selected_output: 0,
        }
    }

    pub(crate) fn graph(&self) -> Option<&ExecutionGraph> {
        if self.selected_proposal().is_some() {
            return None;
        }
        self.graphs.get(self.selected_graph)
    }

    pub(crate) fn selected_proposal(&self) -> Option<&GraphProposalState> {
        self.proposal.as_ref().filter(|_| self.is_proposal_selected)
    }

    pub(crate) fn node(&self) -> Option<&GraphNodeProgress> {
        self.progress.as_ref()?.nodes.get(self.selected_node)
    }

    /// Reconciliation may update other branches, but never retarget a retained
    /// draft.
    pub(crate) fn can_reconcile_editor(&self, refreshed: &Self) -> bool {
        let Some(editor) = &self.editor else {
            return false;
        };
        let same_graph = self
            .graph()
            .zip(refreshed.graph())
            .is_some_and(|(original, current)| {
                original.id == current.id
                    && original.revision == current.revision
                    && original.session_id == current.session_id
            });
        if !same_graph {
            return false;
        }

        match editor.kind {
            GraphEditKind::Reply => {
                refreshed.can_continue()
                    && self
                        .node()
                        .zip(refreshed.node())
                        .is_some_and(|(original, current)| {
                            original.node_id == current.node_id
                                && original.executions.last() == current.executions.last()
                        })
            }
            GraphEditKind::Cancel => refreshed.can_cancel(),
            GraphEditKind::Resume => refreshed.can_resume(),
        }
    }

    /// Focused reviews have a host coordinator and must start a new generation.
    pub(crate) fn is_focused_review(&self) -> bool {
        self.graph().is_some_and(|graph| {
            graph.nodes.iter().any(|node| {
                node.execution.response_profile
                    == ag_protocol::ProtocolRequestProfile::FocusedReview
                    && node.execution.workspace_access == GraphWorkspaceAccess::BackgroundRead
            })
        })
    }

    pub(crate) fn can_resume(&self) -> bool {
        !self.is_focused_review()
            && self
                .progress
                .as_ref()
                .is_some_and(|progress| matches!(progress.status, ExecutionGraphStatus::Paused))
    }

    pub(crate) fn can_cancel(&self) -> bool {
        self.progress.as_ref().is_some_and(|progress| {
            !matches!(
                progress.status,
                ExecutionGraphStatus::Cancelled | ExecutionGraphStatus::Completed
            )
        })
    }

    /// Whether the selected node accepts reply or revision guidance here. A
    /// node awaiting answers asked in session chat; the chat reply answers it.
    pub(crate) fn can_continue(&self) -> bool {
        let Some(graph) = self.graph() else {
            return false;
        };
        let Some(node) = graph.nodes.get(self.selected_node) else {
            return false;
        };
        let Some(progress) = self.progress.as_ref() else {
            return false;
        };
        let Some(current) = self.node() else {
            return false;
        };

        !self.is_focused_review()
            && progress.status != ExecutionGraphStatus::Cancelled
            && matches!(
                current.status,
                GraphNodeStatus::Succeeded | GraphNodeStatus::Failed | GraphNodeStatus::Cancelled
            )
            && !current.executions.is_empty()
            && takes_revision_guidance(&graph.nodes, progress, &node.id)
    }

    pub(crate) fn can_retry(&self) -> bool {
        !self.is_focused_review()
            && self
                .progress
                .as_ref()
                .is_some_and(|progress| progress.status != ExecutionGraphStatus::Cancelled)
            && self.node().is_some_and(|node| {
                matches!(
                    node.status,
                    GraphNodeStatus::Failed | GraphNodeStatus::Cancelled
                ) && !node.executions.is_empty()
            })
    }

    pub(crate) fn can_accept(&self) -> bool {
        self.progress
            .as_ref()
            .is_some_and(|progress| progress.status != ExecutionGraphStatus::Cancelled)
            && self
                .node()
                .is_some_and(|node| node.status == GraphNodeStatus::Succeeded)
            && self
                .outputs
                .get(self.selected_output)
                .is_some_and(|output| output.result.state == GraphResultState::Validated)
    }

    /// Budget names are host-captured. Resume cannot introduce additional
    /// names.
    pub(crate) fn budget_names(&self) -> BTreeSet<String> {
        self.graph()
            .into_iter()
            .flat_map(|graph| &graph.nodes)
            .filter_map(|node| node.execution.shared_budget_id.clone())
            .collect()
    }

    pub(crate) fn parse_budgets(&self, text: &str) -> Result<BTreeMap<String, usize>, String> {
        if text.trim_start().starts_with('{') {
            let budgets: BTreeMap<String, usize> = serde_json::from_str(text)
                .map_err(|_| "Enter a JSON object of budget names and positive limits")?;
            if budgets.values().any(|limit| *limit == 0)
                || budgets.keys().cloned().collect::<BTreeSet<_>>() != self.budget_names()
            {
                return Err(
                    "Supply exactly the displayed budget names with positive limits".into(),
                );
            }

            return Ok(budgets);
        }
        let mut budgets = BTreeMap::new();
        for entry in text.split_whitespace() {
            let (name, value) = entry
                .split_once('=')
                .ok_or("Use budget=positive_number for each budget")?;
            let limit = value
                .parse::<usize>()
                .ok()
                .filter(|limit| *limit > 0)
                .ok_or("Budget limits must be positive integers")?;
            if budgets.insert(name.to_string(), limit).is_some() {
                return Err("Duplicate budget name".into());
            }
        }
        if budgets.keys().cloned().collect::<BTreeSet<_>>() != self.budget_names() {
            return Err("Supply exactly the displayed budget names".into());
        }

        Ok(budgets)
    }
}

/// Display conversion never changes the stored body or acceptance reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GraphViewOutput {
    pub(crate) body: String,
    pub(crate) result: GraphResult,
    display: Option<String>,
}

impl GraphViewOutput {
    pub(crate) fn new(result: GraphResult, body: String) -> Self {
        let schema = result.reference.schema;
        let display = if matches!(
            schema,
            ProtocolRequestProfile::SessionTurn | ProtocolRequestProfile::UtilityPrompt
        ) {
            parse_protocol_response_strict(&body, schema)
                .ok()
                .map(|response| response.to_display_text())
        } else {
            serde_json::from_str::<serde_json::Value>(&body)
                .and_then(|value| serde_json::to_string_pretty(&value))
                .ok()
                .map(|json| format!("```json\n{json}\n```"))
        };

        Self {
            body,
            result,
            display,
        }
    }

    pub(crate) fn display(&self) -> &str {
        self.display.as_deref().unwrap_or(&self.body)
    }
}

/// Lists validated outputs whose acceptance a pending dependent waits for,
/// with their succeeded producer, in producer declaration order. Only running
/// or blocked graphs, which a live driver dispatches, list any.
pub(crate) fn outputs_awaiting_acceptance(
    progress: &ExecutionGraphProgress,
) -> Vec<(&GraphNodeProgress, &GraphResult)> {
    if !matches!(
        progress.status,
        ExecutionGraphStatus::Running | ExecutionGraphStatus::Blocked
    ) {
        return Vec::new();
    }
    let awaited_gates = progress
        .nodes
        .iter()
        .flat_map(|dependent| &dependent.blocked)
        .filter_map(|reason| match reason {
            GraphBlockedReason::Dependency {
                gate: GraphCompletionGate::ResultAccepted { output_name },
                node_id,
            } => Some((node_id.as_str(), output_name.as_str())),
            _ => None,
        })
        .collect::<BTreeSet<_>>();

    progress
        .nodes
        .iter()
        .filter(|producer| producer.status == GraphNodeStatus::Succeeded)
        .flat_map(|producer| {
            producer
                .results
                .iter()
                .filter(|result| {
                    result.state == GraphResultState::Validated
                        && awaited_gates.contains(&(
                            producer.node_id.as_str(),
                            result.reference.output_name.as_str(),
                        ))
                })
                .map(move |result| (producer, result))
        })
        .collect()
}

/// Whether reply guidance can still revise a node's output: the node runs in
/// a task conversation, and no output of it was accepted or reached a started
/// dependent, which storage treats as final.
pub(crate) fn takes_revision_guidance(
    nodes: &[GraphNode],
    progress: &ExecutionGraphProgress,
    node_id: &str,
) -> bool {
    let state = |id: &str| progress.nodes.iter().find(|state| state.node_id == id);
    let is_task_conversation = nodes.iter().any(|node| {
        node.id == node_id
            && matches!(
                node.execution.context,
                GraphContext::TaskConversation { .. }
            )
    });
    let is_accepted = state(node_id).is_some_and(|producer| {
        producer
            .results
            .iter()
            .any(|result| result.state == GraphResultState::Accepted)
    });
    let is_consumed = nodes.iter().any(|dependent| {
        dependent
            .dependencies
            .iter()
            .any(|dependency| dependency.node_id == node_id)
            && state(&dependent.id).is_some_and(|started| !started.executions.is_empty())
    });

    is_task_conversation && !is_accepted && !is_consumed
}

/// Explicit text entry separate from ordinary session chat.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GraphEditor {
    pub(crate) input: InputState,
    pub(crate) kind: GraphEditKind,
    /// A rejected draft must explicitly reconcile its original bindings before
    /// resubmission.
    pub(crate) needs_reconciliation: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraphEditKind {
    Cancel,
    Reply,
    Resume,
}

#[cfg(test)]
#[path = "execution_graph_test.rs"]
pub(crate) mod tests;
