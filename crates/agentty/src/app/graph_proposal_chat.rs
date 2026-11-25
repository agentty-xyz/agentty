//! Starts graph generation from the mode-selected composer.

use ag_session::GenerateGraphProposalRequest;
use uuid::Uuid;

use crate::app::App;
use crate::app::prompt_intent::{PromptSubmission, PromptWorkflowOutcome};

impl App {
    /// Submits the selected graph mode's task literally, without parsing chat
    /// text as graph operations. The host records the request and that
    /// generation started; a rejection is reported in chat and retains the
    /// armed composer draft.
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
        // A graph beside the chat that can still run gains the requested
        // work instead of competing with a second graph in the worktree.
        let result = match self.expandable_graph_target(&session_id).await {
            Ok(expands) => {
                let request = GenerateGraphProposalRequest {
                    expands,
                    id: Uuid::new_v4().to_string(),
                    request: prompt,
                    session_id: session_id.clone(),
                };
                let service = self.session_view_service();

                self.drive_session_request(service.generate_graph_proposal(request))
                    .await
            }
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            self.append_output_for_session(&session_id, &format!("Graph proposal: {error}"))
                .await;

            return PromptWorkflowOutcome::KeepPrompt;
        }

        PromptWorkflowOutcome::ShowSession { session_id }
    }
}

#[cfg(test)]
#[path = "graph_proposal_chat_test.rs"]
mod tests;
