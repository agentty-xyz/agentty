//! Session API graph proposal tests: which sessions and tasks a frontend may
//! ask a proposal for, and the chat trace an admitted request leaves.

use std::sync::Arc;

use ag_contracts::OneShotError;
use ag_session::{
    GenerateGraphProposalRequest, GraphProposalLimits, SessionError as ApiSessionError, SessionId,
    SessionOperation,
};
use ag_worker::MockRunClient;

use crate::app::App;
use crate::app::execution_graph::tests::seed_session;
use crate::app::graph_node_reply::tests::messages;
use crate::domain::session::Status;
use crate::test_support::new_test_app;

/// Seeds `sess` in `status`, staged as a draft when `is_draft`, over a
/// worker whose every run fails, so admitted generations settle at once.
async fn proposal_app(status: Status, is_draft: bool) -> (App, tempfile::TempDir) {
    let (mut app, directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    app.sessions.sessions_mut()[0].is_draft = is_draft;
    *app.sessions.session_handles()["sess"]
        .status
        .lock()
        .expect("status") = status;
    let mut client = MockRunClient::new();
    client
        .expect_submit()
        .returning(|_| Err(OneShotError::new("offline")));
    app.services.set_run_client(Arc::new(client));

    (app, directory)
}

/// Asks the session API for a proposal of `sess` for `task`.
async fn generate(app: &mut App, task: String) -> Result<(), ApiSessionError> {
    let service = app.session_service();
    let request = GenerateGraphProposalRequest::from_text(
        format!("remote-{}", task.len()),
        SessionId::from("sess"),
        task,
    );
    let result = app
        .drive_session_request(async move { service.generate_graph_proposal(request).await })
        .await;
    app.services.wait_for_cleanup_tasks(None).await;

    result
}

#[tokio::test]
async fn prepared_idle_sessions_take_graph_proposals_and_say_so_in_chat() {
    for status in [
        Status::Draft,
        Status::Review,
        Status::AgentReview,
        Status::Watching,
    ] {
        // Arrange
        let (mut app, _directory) = proposal_app(status, false).await;

        // Act
        let supported = app
            .session_service()
            .supports(SessionOperation::GenerateGraphProposal);
        let result = generate(&mut app, "Split the parser".to_string()).await;

        // Assert
        assert!(supported);
        assert_eq!(result, Ok(()), "{status}");
        let messages = messages(&app).await;
        let position = |kind: &str, content: &str| {
            messages
                .iter()
                .position(|message| message.0 == kind && message.1 == content)
        };
        let request = position("user_prompt", "Split the parser").expect("request echoed");
        let notice =
            position("workflow_notice", "Generating execution graph…").expect("generation notice");
        assert!(request < notice, "{status}");
    }
}

#[tokio::test]
async fn busy_unprepared_and_settled_sessions_refuse_graph_proposals() {
    for (status, is_draft) in [
        (Status::InProgress, false),
        (Status::Rebasing, false),
        (Status::Question, false),
        (Status::Done, false),
        (Status::Draft, true),
    ] {
        // Arrange
        let (mut app, _directory) = proposal_app(status, is_draft).await;

        // Act
        let result = generate(&mut app, "Split the parser".to_string()).await;

        // Assert
        assert_eq!(
            result,
            Err(ApiSessionError::Operation(
                "Graph generation requires a prepared idle session".to_string()
            )),
            "{status}"
        );
        assert!(
            messages(&app)
                .await
                .iter()
                .all(|(_, content)| content != "Generating execution graph…"),
            "{status}"
        );
    }
}

#[tokio::test]
async fn blank_and_oversized_tasks_are_refused() {
    // Arrange
    let (mut app, _directory) = proposal_app(Status::Review, false).await;
    let limit = GraphProposalLimits::default().bytes;

    // Act
    let blank = generate(&mut app, "  ".to_string()).await;
    let oversized = generate(&mut app, "a".repeat(limit + 1)).await;
    let largest = generate(&mut app, "a".repeat(limit)).await;

    // Assert
    let refusal = Err(ApiSessionError::Operation(
        "Graph requests require text within host limits".to_string(),
    ));
    assert_eq!(blank, refusal);
    assert_eq!(oversized, refusal);
    assert_eq!(largest, Ok(()));
}
