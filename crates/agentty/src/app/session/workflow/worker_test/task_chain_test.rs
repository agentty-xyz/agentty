use ag_session::{ContinueTaskChainRequest, TaskChainPhase};
use uuid::Uuid;

use crate::app::task_chain::test_support::{ChainFixture, launch_request};

#[tokio::test]
async fn queued_cancellation_followed_by_retry_never_replays_the_old_attempt() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let initial = fixture.register_turn("Initial plan", false);
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .start_plan_then_implement(&fixture.session_id, launch_request()),
        )
        .await
        .expect("launch");
    fixture.settle(&initial).await;
    let source = fixture
        .snapshot()
        .await
        .current_attempt_id()
        .expect("initial")
        .to_string();
    let hold = fixture
        .app
        .sessions
        .pause_session_worker(&fixture.session_id)
        .await
        .expect("worker hold");
    let canceled_id = Uuid::new_v4().to_string();
    let retry_id = Uuid::new_v4().to_string();
    // Only the new request may consume this scripted provider.
    let retry = fixture.register_turn("Retried plan", false);

    // Act
    fixture
        .app
        .drive_session_request(fixture.service.continue_task_chain(
            "workflow",
            ContinueTaskChainRequest {
                attachments: Vec::new(),
                attempt_id: canceled_id.clone(),
                clarification: Some("old guidance".into()),
                expected_attempt_id: source,
            },
        ))
        .await
        .expect("queued revision");
    let awaited_id = canceled_id.clone();
    fixture
        .wait_for(move |chain| chain.current_attempt_id() == Some(awaited_id.as_str()))
        .await;
    fixture
        .app
        .drive_session_request(fixture.service.cancel_task_chain("workflow"))
        .await
        .expect("cancel queued");
    fixture
        .app
        .drive_session_request(fixture.service.continue_task_chain(
            "workflow",
            ContinueTaskChainRequest {
                attachments: Vec::new(),
                attempt_id: retry_id.clone(),
                clarification: None,
                expected_attempt_id: canceled_id,
            },
        ))
        .await
        .expect("explicit new retry");
    drop(hold);
    fixture.settle(&retry).await;
    let chain = fixture.snapshot().await;
    let progress = fixture
        .app
        .services
        .db()
        .graphs()
        .progress("workflow")
        .await
        .expect("progress")
        .expect("graph");

    // Assert
    assert_eq!(progress.nodes[0].executions.len(), 3);
    assert_eq!(chain.current_attempt_id(), Some(retry_id.as_str()));
    assert_eq!(chain.phase, TaskChainPhase::AwaitingPlanAcceptance);
    assert!(retry.prompt().contains("Initial plan"));
    assert!(retry.prompt().contains("Explicitly retry"));
}
