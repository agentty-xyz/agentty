//! Experimental composer commands for generation, inspection, and approval.

use std::fmt::Write as _;

use ag_protocol::GraphProposalOutcome;
use ag_session::{
    GenerateGraphProposalRequest, GraphProposalState, GraphProposalVersionOutcome, SessionId,
};
use uuid::Uuid;

use crate::app::App;
use crate::app::prompt_intent::PromptWorkflowOutcome;
use crate::domain::turn_prompt::TurnPrompt;

impl App {
    /// Routes `/graph` before ordinary chat or task-chain reply admission.
    pub(super) async fn submit_graph_command(
        &mut self,
        session_id: &SessionId,
        prompt: &TurnPrompt,
    ) -> Option<PromptWorkflowOutcome> {
        let text = prompt.text.trim();
        let command = text.strip_prefix("/graph").filter(|remainder| {
            remainder.is_empty() || remainder.starts_with(char::is_whitespace)
        })?;
        let result = self
            .execute_graph_command(session_id, command.trim(), prompt)
            .await;
        let (notice, outcome) = match result {
            Ok(notice) => (
                notice,
                PromptWorkflowOutcome::ShowSession {
                    session_id: session_id.clone(),
                },
            ),
            Err(error) => (
                format!("Graph proposal: {error}"),
                PromptWorkflowOutcome::KeepPrompt,
            ),
        };
        self.append_output_for_session(session_id, &notice).await;

        Some(outcome)
    }

    async fn execute_graph_command(
        &mut self,
        session_id: &SessionId,
        command: &str,
        prompt: &TurnPrompt,
    ) -> Result<String, String> {
        if !self.experimental_multi_agent {
            return Err("enable --experimental-multi-agent to use /graph".into());
        }
        if prompt.has_attachments() {
            return Err("attachment paths are unsupported for graph proposals".into());
        }
        if command.is_empty() || command == "query" {
            let states = self
                .services
                .db()
                .graph_proposals()
                .list(session_id)
                .await
                .map_err(|error| error.to_string())?;
            return Ok(states.last().map_or_else(
                || {
                    "Use /graph <request> to generate a preview. No nodes run before exact-version \
                     approval."
                        .into()
                },
                proposal_preview,
            ));
        }
        let mut parts = command;
        let operation = next_command_token(&mut parts).unwrap_or_default();
        if matches!(
            operation,
            "approve" | "cancel" | "regenerate" | "answer" | "query"
        ) {
            let id = next_command_token(&mut parts).ok_or("Expected proposal ID")?;
            let service = self.session_view_service();
            let state = self
                .drive_session_request(service.get_graph_proposal(id))
                .await
                .map_err(|error| error.to_string())?
                .ok_or("Proposal is missing")?;
            if state.input.session_id != *session_id {
                return Err("Proposal belongs to a different session".into());
            }
            if operation == "query" {
                return Ok(proposal_preview(&state));
            }
            let version = next_command_token(&mut parts)
                .ok_or("Expected the inspected version number")?
                .parse::<u32>()
                .map_err(|_| "Expected the inspected version number")?;
            let result = match operation {
                "approve" => {
                    self.drive_session_request(service.approve_graph_proposal(id, version))
                        .await
                }
                "cancel" => {
                    self.drive_session_request(service.cancel_graph_proposal(id, version))
                        .await
                }
                "regenerate" => {
                    self.drive_session_request(service.regenerate_graph_proposal(id, version))
                        .await
                }
                _ => {
                    let answers: Vec<String> = serde_json::from_str(
                        Some(parts.trim_start())
                            .filter(|answers| !answers.is_empty())
                            .ok_or("Expected a JSON array with all answers")?,
                    )
                    .map_err(|error| error.to_string())?;
                    self.drive_session_request(service.answer_graph_proposal(id, version, answers))
                        .await
                }
            };
            result.map_err(|error| error.to_string())?;
            let current = self
                .drive_session_request(service.get_graph_proposal(id))
                .await
                .map_err(|error| error.to_string())?
                .ok_or("Proposal is missing")?;

            return Ok(proposal_preview(&current));
        }
        self.generate_graph_chat_request(session_id, command, prompt)
            .await
    }

    async fn generate_graph_chat_request(
        &mut self,
        session_id: &SessionId,
        command: &str,
        prompt: &TurnPrompt,
    ) -> Result<String, String> {
        let (id, request_text) = if command.starts_with("new ") || command == "new" {
            let remainder = command.strip_prefix("new").unwrap_or_default().trim_start();
            let (id, request) = remainder
                .split_once(char::is_whitespace)
                .ok_or("Use /graph new <id> <request>")?;
            (id.to_string(), request.trim_start())
        } else {
            (Uuid::new_v4().to_string(), command)
        };
        let request = GenerateGraphProposalRequest {
            id,
            request: TurnPrompt {
                text: request_text.into(),
                ..prompt.clone()
            },
            session_id: session_id.clone(),
        };
        let id = request.id.clone();
        let service = self.session_view_service();
        self.drive_session_request(service.generate_graph_proposal(request))
            .await
            .map_err(|error| error.to_string())?;

        Ok(format!(
            "Graph proposal {id} v1: generating a read-only preview. Use /graph query to inspect \
             progress. No graph nodes have launched."
        ))
    }
}

/// Consumes one whitespace-delimited token without splitting the remaining
/// JSON.
fn next_command_token<'a>(remaining: &mut &'a str) -> Option<&'a str> {
    let text = remaining.trim_start();
    if text.is_empty() {
        return None;
    }
    let (token, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    *remaining = rest;

    Some(token)
}

/// Pure human-readable inspection of a retained version, including its limits.
pub(super) fn proposal_preview(state: &GraphProposalState) -> String {
    let id = &state.input.id;
    let version = state.version.number;
    let mut text = format!(
        "Graph proposal {id} v{version}\nModel: {}/{}; permission: {}\nLimits: {} nodes, {} \
         edges, {} bytes, {} generations, {} provider calls per generation, {} execution provider \
         calls\nRequest: {}\n",
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
            "Approved exact version {version}; graph {}. Press w for execution controls. Graph \
             output acceptance remains separate.",
            approval.graph_id
        );
        return text;
    }
    match &state.version.outcome {
        GraphProposalVersionOutcome::Generating => text.push_str(
            "Generating, or interrupted before completion. Query never restarts generation. \
             Regenerate explicitly to use another captured generation slot.\n",
        ),
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
                let _ = writeln!(
                    text,
                    "/graph answer {id} {version} [\"answer to question 1\"]"
                );
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
                    let _ = writeln!(text, "/graph approve {id} {version}");
                }
            }
        },
    }
    let _ = write!(
        text,
        "/graph regenerate {id} {version}\n/graph cancel {id} {version}\nApproval of this graph \
         does not accept outputs produced later."
    );

    text
}

#[cfg(test)]
#[path = "graph_proposal_chat_test.rs"]
mod tests;
