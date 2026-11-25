//! Bounded snapshot of the visible chat's graph proposal and execution.

use std::fmt::{Debug, Write as _};
use std::time::{Duration, Instant};

use ag_protocol::GraphProposalOutcome;
use ag_session::{
    ExecutionGraphProgress, ExecutionGraphStatus, GraphBlockedReason, GraphCompletionGate,
    GraphContext, GraphDependency, GraphNode, GraphNodeStatus, GraphProposalState,
    GraphProposalVersionOutcome, QuestionItem, SessionGraphApproval, SessionGraphProposalPhase,
    SessionGraphSummary, SessionId, push_numbered_questions,
};

use super::execution_graph::{outputs_awaiting_acceptance, takes_revision_guidance};

/// Short chat report for a settled proposal version. Clarification questions
/// are listed as the record of what was asked and answered in the session
/// question panel; the graph itself is shown only in the sidebar and
/// inspector.
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
                let mut text = format!("Graph proposal v{version} needs your input:");
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
                    let nodes = added_nodes(state, &launch.graph.nodes).len();
                    let noun = if nodes == 1 { "node" } else { "nodes" };
                    let what = if state.base.is_some() {
                        format!("{nodes} new {noun} for the running graph")
                    } else {
                        format!("{nodes} {noun}")
                    };

                    format!(
                        "Graph proposal v{version} ready: {what}. Reply to approve, revise, or \
                         cancel it."
                    )
                },
            ),
        },
    }
}

/// Chat message announcing that a node's output waits for the user's
/// approval, after the output it refers to, with the same reply guidance as
/// the sidebar.
pub(crate) fn output_approval_notice(node_id: &str, output: &str, takes_revision: bool) -> String {
    let action = if takes_revision {
        "reply in chat to accept or revise it"
    } else {
        "reply in chat to accept it"
    };
    let output = output.trim();
    let mut text = String::new();
    if !output.is_empty() {
        let _ = write!(text, "{output}\n\n");
    }
    let _ = write!(text, "`{node_id}` awaits your approval; {action}.");

    text
}

/// Retained proposal details for the scrollable inspector, as Markdown.
///
/// Captured request and instruction text, plus multi-line objectives,
/// generation errors, and clarification questions, is block-quoted so its own
/// Markdown structure, such as fences or headings, cannot reshape the preview.
pub(crate) fn proposal_preview(state: &GraphProposalState) -> String {
    let version = state.version.number;
    let limits = &state.limits;
    let mut text = format!(
        "## Graph proposal v{version}\n\n**Model:** `{}/{}` · **Permission:** {}\n**Limits:** {} \
         nodes · {} edges · {} bytes · {} generations · {} provider calls per generation · {} \
         execution provider calls\n\n### Request\n\n{}\n\n### Status\n\n",
        state.agent.kind(),
        state.agent.model().as_str(),
        state.permission.display_label(),
        limits.nodes,
        limits.edges,
        limits.bytes,
        limits.generations,
        limits.generation_provider_calls,
        limits.provider_calls,
        block_quote(&state.input.request.text)
    );
    if state.version.is_cancelled {
        text.push_str("Cancelled. No graph was launched.");

        return text;
    }
    if let Some(approval) = &state.version.approval {
        let _ = write!(
            text,
            "Approved exact version {version}; graph `{}`. Graph output acceptance remains \
             separate.",
            approval.graph_id
        );

        return text;
    }
    match &state.version.outcome {
        GraphProposalVersionOutcome::Generating => {
            text.push_str("Generating preview. The sidebar updates automatically.\n");
        }
        GraphProposalVersionOutcome::Failed(error) => {
            let _ = writeln!(text, "Generation failed:{}", inline_or_quoted(error));
        }
        GraphProposalVersionOutcome::Generated { launch, response } => match &response.outcome {
            GraphProposalOutcome::Clarification { questions } => {
                text.push_str("Clarification required before preview:");
                push_preview_questions(&mut text, questions);
                text.push('\n');
            }
            GraphProposalOutcome::Proposal { .. } => {
                if let Some(launch) = launch {
                    if let Some(base) = &state.base {
                        let _ = write!(
                            text,
                            "Adds nodes to graph `{}` after revision {}; its {} existing nodes \
                             stay unchanged.\n\n",
                            base.id,
                            base.revision,
                            base.nodes.len()
                        );
                    }
                    push_launch_preview(&mut text, added_nodes(state, &launch.graph.nodes));
                }
            }
        },
    }
    text.push_str(
        "\n---\nReply in chat to approve, revise, or cancel a pending proposal. Approval of this \
         graph does not accept outputs produced later.",
    );

    text
}

/// Human-readable completion gate shared by proposal and execution views.
pub(crate) fn gate_label(gate: &GraphCompletionGate) -> String {
    match gate {
        GraphCompletionGate::ExecutionSucceeded => "successful execution without questions".into(),
        GraphCompletionGate::ResultValidated { output_name } => format!("validated {output_name}"),
        GraphCompletionGate::ResultAccepted { output_name } => format!("accepted {output_name}"),
    }
}

/// Summary row of a cancelled proposal.
pub(crate) const CANCELLED_LABEL: &str = "Cancelled. No graph was launched.";

/// Summary row of an approved proposal whose graph progress is not read yet.
pub(crate) const STARTING_LABEL: &str = "Approved; loading execution status";

/// Summary row of a proposal version that is still generating.
pub(crate) const GENERATING_LABEL: &str = "Generating preview...";

/// Summary row of a proposal waiting for clarification answers.
pub(crate) const NEEDS_INPUT_LABEL: &str = "Needs your input";

/// Summary row of a resolved proposal, and status of a node output, that
/// waits for the user's approval.
pub(crate) const AWAITING_APPROVAL_LABEL: &str = "Awaiting your approval";

/// Summary row of a proposal the host could not resolve.
const UNRESOLVED_LABEL: &str = "Unresolved. Reply to revise or cancel it.";

/// Summary row of a proposal whose generation failed with `error`.
pub(crate) fn generation_failed_label(error: &str) -> String {
    format!("Generation failed: {error}")
}

/// Tells the user how to act on `node_id`'s output that waits for their
/// approval; a reply to a paused graph resumes it before acting.
pub(crate) fn approval_request(node_id: &str, takes_revision: bool, is_paused: bool) -> String {
    let action = if takes_revision {
        "reply in chat to accept or revise it"
    } else {
        "reply in chat to accept it"
    };
    let resume = if is_paused {
        " and resume the graph"
    } else {
        ""
    };

    format!("{node_id} awaits your approval; {action}{resume}")
}

/// User-facing label of an overall graph status.
pub(crate) fn graph_status_label(status: ExecutionGraphStatus) -> &'static str {
    match status {
        ExecutionGraphStatus::Running => "Running",
        ExecutionGraphStatus::Blocked => "Blocked",
        ExecutionGraphStatus::Paused => "Paused",
        ExecutionGraphStatus::Completed => "Completed",
        ExecutionGraphStatus::Failed => "Failed",
        ExecutionGraphStatus::Cancelled => "Canceled",
    }
}

/// Status of one node as the graph views show it: its live state, or a
/// pending acceptance of its output by the user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NodeStatus {
    /// A dependent waits for the user to accept this node's output.
    AwaitingApproval,
    /// Live execution state reported by the graph.
    Live(GraphNodeStatus),
}

impl NodeStatus {
    /// Returns the user-facing label for this status.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::AwaitingApproval => AWAITING_APPROVAL_LABEL,
            Self::Live(GraphNodeStatus::Pending) => "Waiting",
            Self::Live(GraphNodeStatus::Queued | GraphNodeStatus::Running) => "Working",
            Self::Live(GraphNodeStatus::AwaitingAnswers) => "Needs your input",
            Self::Live(GraphNodeStatus::Succeeded) => "Done",
            Self::Live(GraphNodeStatus::Failed) => "Failed",
            Self::Live(GraphNodeStatus::Cancelled) => "Canceled",
        }
    }
}

/// Names what a dependency waits for, omitting the source of an output gate
/// when it is the node drawn directly above.
pub(crate) fn edge_label(dependency: &GraphDependency, previous: Option<&str>) -> String {
    let source = dependency.node_id.as_str();
    let from = if previous == Some(source) {
        String::new()
    } else {
        format!(" from {source}")
    };

    match &dependency.gate {
        GraphCompletionGate::ExecutionSucceeded => format!("after {source}"),
        GraphCompletionGate::ResultValidated { output_name } => {
            format!("needs {output_name}{from}")
        }
        GraphCompletionGate::ResultAccepted { output_name } => {
            format!("needs {output_name} accepted{from}")
        }
    }
}

/// A dependency is satisfied once its source succeeded and the dependent no
/// longer reports it as a blocking gate.
pub(crate) fn is_dependency_satisfied(
    progress: Option<&ExecutionGraphProgress>,
    node: &GraphNode,
    dependency: &GraphDependency,
) -> bool {
    let Some(progress) = progress else {
        return false;
    };
    let source_succeeded = progress.nodes.iter().any(|state| {
        state.node_id == dependency.node_id && state.status == GraphNodeStatus::Succeeded
    });

    source_succeeded
        && progress
            .nodes
            .iter()
            .find(|state| state.node_id == node.id)
            .is_some_and(|state| {
                !state.blocked.iter().any(|reason| {
                    matches!(
                        reason,
                        GraphBlockedReason::Dependency { node_id, .. } if *node_id == dependency.node_id
                    )
                })
            })
}

/// Where `state`'s current version stands, with the summary row the
/// sidebar shows for it before its graph's progress is read.
pub(crate) fn proposal_standing(state: &GraphProposalState) -> (SessionGraphProposalPhase, String) {
    let version = &state.version;
    if version.is_cancelled {
        return (SessionGraphProposalPhase::Cancelled, CANCELLED_LABEL.into());
    }
    if version.approval.is_some() {
        return (SessionGraphProposalPhase::Approved, STARTING_LABEL.into());
    }

    match &version.outcome {
        GraphProposalVersionOutcome::Generating => (
            SessionGraphProposalPhase::Generating,
            GENERATING_LABEL.into(),
        ),
        GraphProposalVersionOutcome::Failed(error) => (
            SessionGraphProposalPhase::Failed,
            generation_failed_label(error),
        ),
        GraphProposalVersionOutcome::Generated { launch, response } => match &response.outcome {
            GraphProposalOutcome::Clarification { .. } => (
                SessionGraphProposalPhase::NeedsInput,
                NEEDS_INPUT_LABEL.into(),
            ),
            GraphProposalOutcome::Proposal { .. } if launch.is_some() => (
                SessionGraphProposalPhase::AwaitingApproval,
                AWAITING_APPROVAL_LABEL.into(),
            ),
            GraphProposalOutcome::Proposal { .. } => (
                SessionGraphProposalPhase::Unresolved,
                UNRESOLVED_LABEL.into(),
            ),
        },
    }
}

/// What the chat summarizes about the latest `proposal` and, once
/// approved, the live `progress` of the graph it launched, with the
/// sidebar's wording. A cancelled proposal reports no progress.
pub(crate) fn graph_summary(
    proposal: &GraphProposalState,
    progress: Option<&ExecutionGraphProgress>,
) -> SessionGraphSummary {
    let (phase, label) = proposal_standing(proposal);
    let nodes = launched_graph_nodes(proposal);
    let mut summary = SessionGraphSummary {
        awaiting_approval: Vec::new(),
        done: 0,
        label,
        phase,
        status: None,
        total: nodes.len(),
        version: proposal.version.number,
    };
    let Some(progress) = progress.filter(|_| phase != SessionGraphProposalPhase::Cancelled) else {
        return summary;
    };
    let is_paused = progress.status == ExecutionGraphStatus::Paused;
    summary.awaiting_approval = outputs_awaiting_acceptance(progress)
        .into_iter()
        .map(|(producer, _)| {
            let takes_revision = takes_revision_guidance(nodes, progress, &producer.node_id);

            SessionGraphApproval {
                node_id: producer.node_id.clone(),
                takes_revision,
                text: approval_request(&producer.node_id, takes_revision, is_paused),
            }
        })
        .collect();
    summary.done = progress
        .nodes
        .iter()
        .filter(|state| state.status == GraphNodeStatus::Succeeded)
        .count();
    summary.label = graph_status_label(progress.status).to_string();
    summary.status = Some(progress.status);

    summary
}

/// Returns the resolved nodes of a proposal that is not cancelled, or none.
pub(crate) fn launched_graph_nodes(state: &GraphProposalState) -> &[GraphNode] {
    match &state.version.outcome {
        GraphProposalVersionOutcome::Generated {
            launch: Some(launch),
            ..
        } if !state.version.is_cancelled => launch.graph.nodes.as_slice(),
        _ => &[],
    }
}

/// The resolved `nodes` that `state` proposes: all of them for a new graph,
/// or those after its captured base for an expansion.
fn added_nodes<'a>(state: &GraphProposalState, nodes: &'a [GraphNode]) -> &'a [GraphNode] {
    let existing = state.base.as_ref().map_or(0, |base| base.nodes.len());

    nodes.get(existing..).unwrap_or_default()
}

/// Appends numbered clarification questions, quoting multi-line question text
/// like other captured text.
fn push_preview_questions(text: &mut String, questions: &[QuestionItem]) {
    for (index, question) in questions.iter().enumerate() {
        let mut question_text = question.text.clone();
        if !question.options.is_empty() {
            let _ = write!(question_text, " ({})", question.options.join(" / "));
        }
        let _ = write!(text, "\n{}.{}", index + 1, inline_or_quoted(&question_text));
    }
}

/// Appends one section per proposed node, keeping long captured
/// instructions after the short summary fields.
fn push_launch_preview(text: &mut String, nodes: &[GraphNode]) {
    let _ = write!(
        text,
        "Awaiting approval of this exact version. Nodes have not launched.\n\n### Nodes ({})\n",
        nodes.len()
    );
    for (index, node) in nodes.iter().enumerate() {
        let execution = &node.execution;
        let dependencies = node
            .dependencies
            .iter()
            .map(|dependency| {
                format!(
                    "`{}` ({})",
                    inline_literal(&dependency.node_id),
                    inline_literal(&gate_label(&dependency.gate))
                )
            })
            .collect::<Vec<_>>();
        let inputs = node
            .inputs
            .iter()
            .map(|input| {
                format!(
                    "`{}` ← `{}.{}`",
                    inline_literal(&input.name),
                    inline_literal(&input.producer_node_id),
                    inline_literal(&input.output_name)
                )
            })
            .collect::<Vec<_>>();
        let outputs = node
            .outputs
            .iter()
            .map(|output| {
                format!(
                    "`{}` ({})",
                    inline_literal(&output.name),
                    variant_words(&output.schema)
                )
            })
            .collect::<Vec<_>>();
        let context = match &execution.context {
            GraphContext::Fresh => "fresh".to_string(),
            GraphContext::TaskConversation { task_id } => {
                format!("task `{}`", inline_literal(task_id))
            }
        };
        let _ = write!(
            text,
            "\n#### {}. {} · {}\n\n- **Objective:**{}\n- **Access:** {} · **Context:** {} · \
             **Effects:** {} · **Schema:** {}\n- **Requires:** {}\n- **Inputs:** {}\n- \
             **Outputs:** {}\n\n**Captured instructions** (`{}`)\n\n{}\n",
            index + 1,
            inline_literal(&node.id),
            inline_literal(&node.definition.name),
            inline_or_quoted(&node.objective),
            variant_words(&execution.workspace_access),
            context,
            variant_words(&execution.post_processing),
            variant_words(&execution.response_profile),
            list_or_none(&dependencies),
            list_or_none(&inputs),
            list_or_none(&outputs),
            inline_literal(&node.definition.id),
            block_quote(&node.definition.prompt)
        );
    }
}

/// Keeps a captured name on one line and neutralizes backticks, so an
/// identifier such as `inspect\n```text` cannot open a fence or end its code
/// span. Line breaks and other control characters become spaces, and each
/// backtick becomes the look-alike `ˋ`.
fn inline_literal(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '`' => 'ˋ',
            character if character.is_control() => ' ',
            character => character,
        })
        .collect()
}

/// Keeps one-line captured text inline after its label and block-quotes
/// multi-line text so its own Markdown, such as a code fence, cannot reshape
/// the preview.
fn inline_or_quoted(value: &str) -> String {
    if value.contains('\n') {
        return format!("\n\n{}\n", block_quote(value));
    }

    format!(" {value}")
}

/// Prefixes every line so captured text renders as an inert quote.
fn block_quote(text: &str) -> String {
    text.lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Spells a unit enum variant such as `SharedRead` as `shared read`.
fn variant_words(value: &impl Debug) -> String {
    format!("{value:?}")
        .chars()
        .enumerate()
        .fold(String::new(), |mut words, (index, character)| {
            if character.is_uppercase() && index > 0 {
                words.push(' ');
            }
            words.push(character.to_ascii_lowercase());

            words
        })
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        return "none".into();
    }

    items.join(", ")
}

/// Latest proposal and live execution status shown beside regular chat.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GraphChatSnapshot {
    pub(crate) error: Option<String>,
    pub(crate) progress: Option<ExecutionGraphProgress>,
    pub(crate) proposal: GraphProposalState,
}

impl GraphChatSnapshot {
    /// Returns whether the sidebar animates this snapshot: a preview is
    /// generating, the launched graph is running, or any node is still queued
    /// or running (a graph reports `Failed` while siblings keep working).
    pub(crate) fn is_animating(&self) -> bool {
        let version = &self.proposal.version;
        if version.is_cancelled {
            return false;
        }

        matches!(version.outcome, GraphProposalVersionOutcome::Generating)
            || self.progress.as_ref().is_some_and(|progress| {
                progress.status == ExecutionGraphStatus::Running
                    || progress.nodes.iter().any(|node| {
                        matches!(
                            node.status,
                            GraphNodeStatus::Queued | GraphNodeStatus::Running
                        )
                    })
            })
    }
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
