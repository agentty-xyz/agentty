use std::sync::Arc;

use ag_protocol::TurnPrompt;
use ag_store::{DbError, MockGraphProposalRepository};

use crate::app::execution_graph::tests::seed_session;
use crate::app::prompt_intent::{PromptSessionMode, PromptSubmission, PromptWorkflowOutcome};
use crate::test_support::new_test_app;

#[tokio::test]
async fn armed_graph_requests_preserve_literal_task_text_and_ignore_empty_input() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    app.experimental_multi_agent = true;
    let mut proposals = MockGraphProposalRepository::new();
    proposals.expect_get().times(1).returning(|_, _| Ok(None));
    proposals
        .expect_create()
        .times(1)
        .withf(|state| state.input.request.text == "query the architecture")
        .returning(|_| {
            Err(DbError::InvalidData {
                entity: "proposal",
                reason: "save failed".into(),
            })
        });
    app.services
        .set_graph_proposal_repository(Arc::new(proposals));

    // Act / Assert
    for text in ["", "query the architecture"] {
        let outcome = app
            .submit_graph_request(PromptSubmission {
                prompt: TurnPrompt::from_text(text.into()),
                session_id: "sess".into(),
                session_mode: PromptSessionMode::Existing,
            })
            .await;
        assert_eq!(outcome, PromptWorkflowOutcome::KeepPrompt);
    }
}
