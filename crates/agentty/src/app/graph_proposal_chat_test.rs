//! Graph generation unit tests for the mode-selected composer.

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
    let mut proposals = MockGraphProposalRepository::new();
    proposals.expect_get().times(1).returning(|_, _| Ok(None));
    proposals
        .expect_list()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    proposals
        .expect_create()
        .times(1)
        .withf(|state| {
            state.input.request.text == "query the architecture" && state.input.expands.is_none()
        })
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

#[tokio::test]
async fn graph_requests_keep_the_draft_when_the_graph_beside_the_chat_is_unreadable() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let mut proposals = MockGraphProposalRepository::new();
    proposals.expect_list().times(1).returning(|_| {
        Err(DbError::InvalidData {
            entity: "proposal",
            reason: "unreadable".into(),
        })
    });
    proposals.expect_create().never();
    app.services
        .set_graph_proposal_repository(Arc::new(proposals));

    // Act
    let outcome = app
        .submit_graph_request(PromptSubmission {
            prompt: TurnPrompt::from_text("query the architecture".into()),
            session_id: "sess".into(),
            session_mode: PromptSessionMode::Existing,
        })
        .await;

    // Assert
    assert_eq!(outcome, PromptWorkflowOutcome::KeepPrompt);
}
