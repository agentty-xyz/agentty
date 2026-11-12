//! Bounded snapshot of the visible chat's graph proposal and execution.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use ag_protocol::GraphProposalOutcome;
use ag_session::{
    ExecutionGraphProgress, GraphProposalState, GraphProposalVersionOutcome, SessionId,
    push_numbered_questions,
};

/// Short chat report for a settled proposal version. Clarification questions
/// stay in chat; the graph itself is shown only in the sidebar and inspector.
pub(crate) fn proposal_chat_notice(state: &GraphProposalState) -> String {
    let version = state.version.number;
    if state.version.is_cancelled {
        return format!("Graph proposal v{version} cancelled.");
    }
    match &state.version.outcome {
        GraphProposalVersionOutcome::Generating => format!("Generating graph proposal v{version}…"),
        GraphProposalVersionOutcome::Failed(error) => {
            format!("Graph proposal v{version} failed: {error}. Reply to revise or cancel it.")
        }
        GraphProposalVersionOutcome::Generated { launch, response } => match &response.outcome {
            GraphProposalOutcome::Clarification { questions } => {
                let mut text = "The graph needs your input. Reply in chat to answer:".to_string();
                push_numbered_questions(&mut text, questions);

                text
            }
            GraphProposalOutcome::Proposal { .. } => launch.as_ref().map_or_else(
                || {
                    format!(
                        "Graph proposal v{version} is unresolved. Reply to revise or cancel it."
                    )
                },
                |launch| {
                    let nodes = launch.graph.nodes.len();
                    let noun = if nodes == 1 { "node" } else { "nodes" };

                    format!(
                        "Graph proposal v{version} ready: {nodes} {noun}. Reply to approve, \
                         revise, or cancel it."
                    )
                },
            ),
        },
    }
}

/// Retained proposal details for the scrollable inspector.
pub(crate) fn proposal_preview(state: &GraphProposalState) -> String {
    let version = state.version.number;
    let mut text = format!(
        "Graph proposal {} v{version}\nModel: {}/{}; permission: {}\nLimits: {} nodes, {} edges, \
         {} bytes, {} generations, {} provider calls per generation, {} execution provider \
         calls\nRequest: {}\n",
        state.input.id,
        state.agent.kind(),
        state.agent.model().as_str(),
        state.permission,
        state.limits.nodes,
        state.limits.edges,
        state.limits.bytes,
        state.limits.generations,
        state.limits.generation_provider_calls,
        state.limits.provider_calls,
        state.input.request.text
    );
    if state.version.is_cancelled {
        text.push_str("Cancelled. No graph was launched.");
        return text;
    }
    if let Some(approval) = &state.version.approval {
        let _ = write!(
            text,
            "Approved exact version {version}; graph {}. Graph output acceptance remains separate.",
            approval.graph_id
        );
        return text;
    }
    match &state.version.outcome {
        GraphProposalVersionOutcome::Generating => {
            text.push_str("Generating preview. The sidebar updates automatically.\n");
        }
        GraphProposalVersionOutcome::Failed(error) => {
            let _ = writeln!(text, "Generation failed: {error}");
        }
        GraphProposalVersionOutcome::Generated { launch, response } => match &response.outcome {
            GraphProposalOutcome::Clarification { questions } => {
                text.push_str("Clarification required before preview:\n");
                for (index, question) in questions.iter().enumerate() {
                    let _ = writeln!(
                        text,
                        "{}. {} {:?}",
                        index + 1,
                        question.text,
                        question.options
                    );
                }
            }
            GraphProposalOutcome::Proposal { .. } => {
                if let Some(launch) = launch {
                    text.push_str(
                        "Preview awaiting exact-version approval. Nodes have not launched.\n",
                    );
                    for node in &launch.graph.nodes {
                        let _ = writeln!(
                            text,
                            "{}: {}\n  Agent: {} ({})\n  Captured instructions:\n{}\n  Access: \
                             {:?}; context: {:?}; effects: {:?}; schema: {:?}\n  Requires: {:?}\n  \
                             Inputs: {:?}\n  Outputs: {:?}",
                            node.id,
                            node.objective,
                            node.definition.name,
                            node.definition.id,
                            node.definition.prompt,
                            node.execution.workspace_access,
                            node.execution.context,
                            node.execution.post_processing,
                            node.execution.response_profile,
                            node.dependencies,
                            node.inputs,
                            node.outputs
                        );
                    }
                }
            }
        },
    }
    text.push_str(
        "Reply in chat to approve, revise, or cancel a pending proposal. Approval of this graph \
         does not accept outputs produced later.",
    );

    text
}

/// Latest proposal and live execution status shown beside regular chat.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GraphChatSnapshot {
    pub(crate) error: Option<String>,
    pub(crate) progress: Option<ExecutionGraphProgress>,
    pub(crate) proposal: GraphProposalState,
}

/// Keeps only the visible session. Session updates and proposal creation
/// invalidate the snapshot; proposals and failed reads also refresh once per
/// second for live status.
#[derive(Debug, Default)]
pub(crate) struct GraphChatCache {
    is_stale: bool,
    refreshed_at: Option<Instant>,
    retry: bool,
    session_id: Option<SessionId>,
    snapshot: Option<GraphChatSnapshot>,
    update_version: u64,
}

impl GraphChatCache {
    pub(crate) fn snapshot(&self, session_id: &str) -> Option<&GraphChatSnapshot> {
        self.snapshot
            .as_ref()
            .filter(|_| self.session_id.as_ref().is_some_and(|id| id == session_id))
    }

    /// Forces the next refresh, for proposal writes that do not change the
    /// session update version.
    pub(crate) fn invalidate(&mut self) {
        self.is_stale = true;
    }

    pub(crate) fn needs_refresh(&self, session_id: &str, version: u64, now: Instant) -> bool {
        self.is_stale
            || self.session_id.as_ref().is_none_or(|id| id != session_id)
            || self.update_version != version
            || ((self.snapshot.is_some() || self.retry)
                && self
                    .refreshed_at
                    .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(1)))
    }

    /// Retains the last snapshot on failure, clearly marking it as stale.
    pub(crate) fn complete(
        &mut self,
        session_id: SessionId,
        version: u64,
        now: Instant,
        result: Result<Option<GraphChatSnapshot>, String>,
    ) -> bool {
        if self.session_id.as_ref() != Some(&session_id) {
            self.snapshot = None;
        }
        self.is_stale = false;
        self.session_id = Some(session_id);
        self.update_version = version;
        self.refreshed_at = Some(now);
        self.retry = result.is_err();
        match result {
            Ok(snapshot) => {
                let changed = self.snapshot != snapshot;
                self.snapshot = snapshot;

                changed
            }
            Err(error) => {
                if let Some(snapshot) = &mut self.snapshot {
                    snapshot.error = Some(error);
                }

                true
            }
        }
    }
}

#[cfg(test)]
#[path = "graph_chat_test.rs"]
mod tests;
