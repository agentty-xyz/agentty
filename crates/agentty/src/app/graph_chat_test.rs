use std::sync::Arc;

use ag_session::{GraphProposalApproval, test_support};
use ag_store::{DbError, MockGraphProposalRepository, MockGraphRepository};

use crate::domain::session::SessionId;
use crate::presentation::app_mode::AppMode;
use crate::test_support::new_test_app;

#[tokio::test]
async fn graph_sidebar_loads_saved_proposals_only_for_the_visible_enabled_session() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let session_id = SessionId::from("graph-chat");
    app.services
        .db()
        .sessions()
        .insert_session(
            &session_id,
            "gpt-6.1-sol",
            "main",
            "Review",
            app.active_project_id(),
        )
        .await
        .expect("session");
    let proposal = test_support::graph_proposal_state("proposal", &session_id);
    app.services
        .db()
        .graph_proposals()
        .create(&proposal)
        .await
        .expect("proposal");
    app.mode = AppMode::View {
        session_id: session_id.clone(),
        scroll_offset: None,
    };

    // Act / Assert
    assert!(!app.refresh_graph_chat_if_needed().await);
    app.experimental_graph = true;
    assert!(app.refresh_graph_chat_if_needed().await);
    assert_eq!(
        app.graph_chat
            .snapshot(&session_id)
            .expect("graph snapshot")
            .proposal,
        proposal
    );
    assert!(!app.refresh_graph_chat_if_needed().await);
    app.mode = AppMode::List;
    assert!(!app.refresh_graph_chat_if_needed().await);
    app.mode = AppMode::View {
        session_id: "other".into(),
        scroll_offset: None,
    };
    assert!(!app.refresh_graph_chat_if_needed().await);
    assert!(app.graph_chat.snapshot("other").is_none());
}

#[tokio::test]
async fn graph_sidebar_reads_approved_progress_and_reports_repository_failures() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let mut proposal = test_support::graph_proposal_state("proposal", "session");
    proposal.version.approval = Some(GraphProposalApproval {
        graph_id: "graph".into(),
        limits: proposal.limits.clone(),
        version: 1,
    });
    let mut proposals = MockGraphProposalRepository::new();
    proposals
        .expect_latest()
        .times(2)
        .returning(move |_| Ok(Some(proposal.clone())));
    app.services
        .set_graph_proposal_repository(Arc::new(proposals));
    let mut graphs = MockGraphRepository::new();
    graphs.expect_progress().times(1).returning(|_| Ok(None));
    app.services.set_graph_repository(Arc::new(graphs));

    // Act / Assert
    let snapshot = app
        .load_graph_chat(&"session".into())
        .await
        .expect("graph load")
        .expect("proposal");
    assert!(snapshot.proposal.version.approval.is_some());
    assert!(snapshot.progress.is_none());

    // Arrange
    let mut graphs = MockGraphRepository::new();
    graphs.expect_progress().times(1).returning(|_| {
        Err(DbError::InvalidData {
            entity: "graph",
            reason: "offline".into(),
        })
    });
    app.services.set_graph_repository(Arc::new(graphs));

    // Act / Assert
    assert!(
        app.load_graph_chat(&"session".into())
            .await
            .expect_err("failed progress read")
            .contains("offline")
    );

    // Arrange
    let mut proposals = MockGraphProposalRepository::new();
    proposals.expect_latest().times(1).returning(|_| {
        Err(DbError::InvalidData {
            entity: "proposal",
            reason: "offline".into(),
        })
    });
    app.services
        .set_graph_proposal_repository(Arc::new(proposals));

    // Act / Assert
    assert!(
        app.load_graph_chat(&"session".into())
            .await
            .expect_err("failed proposal read")
            .contains("offline")
    );
}
