//! Bounded snapshot of the visible chat's graph proposal and execution.

use std::fmt::{Debug, Write as _};
use std::time::{Duration, Instant};

use ag_protocol::GraphProposalOutcome;
use ag_session::{
    ExecutionGraphProgress, GraphCompletionGate, GraphContext, GraphNode, GraphProposalState,
    GraphProposalVersionOutcome, QuestionItem, SessionId, push_numbered_questions,
};

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
                    push_launch_preview(&mut text, &launch.graph.nodes);
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
