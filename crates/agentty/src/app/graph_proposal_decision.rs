//! Model decision for a chat reply to a pending graph proposal.

use std::fmt::Write as _;
use std::path::PathBuf;

use ag_protocol::GraphProposalOutcome;
use ag_session::{AgentSelection, GraphProposalState, GraphProposalVersionOutcome};
use ag_worker::RunClient;
use askama::Template;
use tracing::warn;

use crate::domain::agent::{ReasoningLevel, SpeedMode};

/// Askama view model whose inputs are already JSON-encoded strings.
#[derive(Template)]
#[template(path = "graph_proposal_reply_decision_prompt.md", escape = "none")]
struct GraphProposalReplyDecisionPromptTemplate<'a> {
    proposal: &'a str,
    reply: &'a str,
}

/// Proposal action a chat reply resolves to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraphProposalReply {
    /// Approve the exact inspected version and start execution.
    Approve,
    /// Discard the proposal permanently.
    Cancel,
    /// Regenerate with the reply as answers or feedback.
    Revise,
}

/// Inputs for deciding what a reply means for one inspected proposal.
pub(crate) struct GraphProposalReplyDecision {
    /// Provider and model captured by the proposal.
    pub(crate) agent: AgentSelection,
    /// Whether the inspected version has a resolved, approvable preview.
    pub(crate) can_approve: bool,
    /// Session workspace used as the read-only run folder.
    pub(crate) folder: PathBuf,
    /// Compact description of the inspected version.
    pub(crate) proposal: String,
    /// User-authored reply to classify.
    pub(crate) reply: String,
}

impl GraphProposalReplyDecision {
    /// Captures the inspected version's selection and state for one reply.
    pub(crate) fn new(state: &GraphProposalState, folder: PathBuf, reply: String) -> Self {
        Self {
            agent: state.agent,
            can_approve: matches!(
                state.version.outcome,
                GraphProposalVersionOutcome::Generated {
                    launch: Some(_),
                    ..
                }
            ),
            folder,
            proposal: proposal_summary(state),
            reply,
        }
    }

    /// Decides through one read-only utility run on the Run Worker. Run
    /// failures, unparsable output, unknown words, and approval of a version
    /// without a resolved preview revise the proposal, so a graph never runs
    /// or disappears on an uncertain decision.
    pub(crate) async fn decide(&self, run_client: &dyn RunClient) -> GraphProposalReply {
        let submission = run_client
            .submit(ag_contracts::OneShotRequest {
                activity_tx: None,
                child_pid: None,
                execution_policy: ag_contracts::ExecutionPolicy::default(),
                folder: self.folder.clone(),
                harness: self.agent.kind().to_string(),
                model: self.agent.model().as_str().to_string(),
                permission_mode: ag_contracts::PermissionMode::ReadOnly,
                prompt: self.prompt(),
                provider_call_budget: None,
                reasoning_level: ReasoningLevel::default(),
                request_kind: ag_contracts::AgentRequestKind::UtilityPrompt,
                speed_mode: SpeedMode::Normal,
            })
            .await;
        let answer = match submission {
            Ok(submission) => submission.response.to_answer_display_text(),
            Err(error) => {
                warn!(error = %error, "graph proposal reply decision failed; revising");

                return GraphProposalReply::Revise;
            }
        };
        let reply = Self::parse(&answer).unwrap_or_else(|| {
            let answer_bytes = answer.len();
            warn!(answer_bytes, "unknown graph proposal decision; revising");

            GraphProposalReply::Revise
        });

        if reply == GraphProposalReply::Approve && !self.can_approve {
            return GraphProposalReply::Revise;
        }

        reply
    }

    /// Renders the task contract with proposal and reply passed as JSON data.
    fn prompt(&self) -> String {
        let proposal = serde_json::json!(self.proposal).to_string();
        let reply = serde_json::json!(self.reply).to_string();
        let template = GraphProposalReplyDecisionPromptTemplate {
            proposal: &proposal,
            reply: &reply,
        };

        // Rendering borrowed strings into a `String` cannot fail. A blank
        // prompt would still degrade safely to a revision.
        template.render().unwrap_or_default()
    }

    /// Accepts only the three contract words, tolerating surrounding quotes,
    /// code ticks, a trailing period, and letter case.
    fn parse(answer: &str) -> Option<GraphProposalReply> {
        match answer
            .trim()
            .trim_matches(['`', '"', '\'', '.'])
            .to_ascii_lowercase()
            .as_str()
        {
            "approve" => Some(GraphProposalReply::Approve),
            "cancel" => Some(GraphProposalReply::Cancel),
            "revise" => Some(GraphProposalReply::Revise),
            _ => None,
        }
    }
}

/// Describes the inspected version: its status, whether it can be approved,
/// and the questions or nodes the reply may refer to.
fn proposal_summary(state: &GraphProposalState) -> String {
    let mut text = format!(
        "Version {}. Request: {}\n",
        state.version.number, state.input.request.text
    );
    match &state.version.outcome {
        GraphProposalVersionOutcome::Generating => {
            text.push_str("Status: generating a preview; it cannot be approved yet.");
        }
        GraphProposalVersionOutcome::Failed(error) => {
            let _ = write!(
                text,
                "Status: generation failed ({error}); it cannot be approved."
            );
        }
        GraphProposalVersionOutcome::Generated { launch, response } => match &response.outcome {
            GraphProposalOutcome::Clarification { questions } => {
                text.push_str(
                    "Status: needs answers before a graph can be proposed; it cannot be \
                     approved.\nQuestions:",
                );
                for (index, question) in questions.iter().enumerate() {
                    let _ = write!(text, "\n{}. {}", index + 1, question.text);
                    if !question.options.is_empty() {
                        let _ = write!(text, " ({})", question.options.join(" / "));
                    }
                }
            }
            GraphProposalOutcome::Proposal { .. } => match launch {
                Some(launch) => {
                    text.push_str("Status: ready; it can be approved.\nNodes:");
                    for node in &launch.graph.nodes {
                        let _ = write!(
                            text,
                            "\n- {}: {} ({})",
                            node.id, node.objective, node.definition.name
                        );
                    }
                }
                None => text.push_str("Status: unresolved; it cannot be approved."),
            },
        },
    }

    text
}

#[cfg(test)]
#[path = "graph_proposal_decision_test.rs"]
pub(crate) mod tests;
