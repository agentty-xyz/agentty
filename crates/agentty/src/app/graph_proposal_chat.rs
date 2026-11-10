//! Starts graph generation from the mode-selected composer.

use ag_session::GenerateGraphProposalRequest;
use uuid::Uuid;

use crate::app::App;
use crate::app::prompt_intent::{PromptSubmission, PromptWorkflowOutcome};

impl App {
    /// Submits the selected graph mode's task literally, without parsing chat
    /// text as graph operations. Rejections retain the armed composer draft.
    pub(crate) async fn submit_graph_request(
        &mut self,
        submission: PromptSubmission,
    ) -> PromptWorkflowOutcome {
        let PromptSubmission {
            prompt, session_id, ..
        } = submission;
        if prompt.text.trim().is_empty() {
            return PromptWorkflowOutcome::KeepPrompt;
        }
        let request = GenerateGraphProposalRequest {
            id: Uuid::new_v4().to_string(),
            request: prompt,
            session_id: session_id.clone(),
        };
        let service = self.session_view_service();
        let result = self
            .drive_session_request(service.generate_graph_proposal(request))
            .await;
        let (notice, outcome) = match result {
            Ok(()) => (
                "Generating execution graph…".into(),
                PromptWorkflowOutcome::ShowSession {
                    session_id: session_id.clone(),
                },
            ),
            Err(error) => (
                format!("Graph proposal: {error}"),
                PromptWorkflowOutcome::KeepPrompt,
            ),
        };
        self.append_output_for_session(&session_id, &notice).await;

        outcome
    }
}

#[cfg(test)]
#[path = "graph_proposal_chat_test.rs"]
mod tests;
