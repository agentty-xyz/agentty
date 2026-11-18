//! Steering inbox delivery and closure tests.

use ag_protocol::TurnPrompt;

use super::{
    TurnSteeringInbox, TurnSteeringRegistry, await_acknowledgement, turn_not_steerable_error,
};
use crate::app_server::AppServerError;

#[tokio::test]
async fn inbox_delivers_prompts_in_submission_order() {
    // Arrange
    let inbox = TurnSteeringInbox::new();
    let _first_ack = inbox.submit(TurnPrompt::from("first")).expect("open inbox");
    let _second_ack = inbox
        .submit(TurnPrompt::from("second"))
        .expect("open inbox");

    // Act
    let first = inbox.next().await;
    let second = inbox.next().await;

    // Assert
    assert_eq!(first.prompt.text, "first");
    assert_eq!(second.prompt.text, "second");
}

#[tokio::test]
async fn waiting_turn_wakes_for_a_later_prompt() {
    // Arrange
    let inbox = std::sync::Arc::new(TurnSteeringInbox::new());
    let waiter = tokio::spawn({
        let inbox = std::sync::Arc::clone(&inbox);

        async move { inbox.next().await.prompt.text }
    });
    tokio::task::yield_now().await;

    // Act
    let _ack = inbox.submit(TurnPrompt::from("late")).expect("open inbox");

    // Assert
    assert_eq!(waiter.await.expect("waiter"), "late");
}

#[tokio::test]
async fn closing_rejects_undelivered_and_later_prompts() {
    // Arrange
    let inbox = TurnSteeringInbox::new();
    let acknowledgement = inbox
        .submit(TurnPrompt::from("pending"))
        .expect("open inbox");

    // Act
    inbox.close();
    let late = inbox.submit(TurnPrompt::from("late"));

    // Assert
    let pending = acknowledgement.await.expect("reply");
    assert_eq!(
        pending.expect_err("closed").to_string(),
        turn_not_steerable_error().to_string()
    );
    assert!(late.is_err());
}

#[tokio::test]
async fn registry_routes_prompts_only_while_the_turn_is_registered() {
    // Arrange
    let registry = TurnSteeringRegistry::default();
    let registration = registry.register("session-1");
    let inbox = registration.inbox();
    let acknowledgement = registry.submit("session-1", "also add tests".into());
    let missing = registry.submit("session-2", "hello".into());

    // Act
    let delivered = inbox.next().await;
    let delivered_text = delivered.prompt.text.clone();
    delivered.acknowledge(Ok(()));
    drop(registration);
    let after_turn = registry.submit("session-1", "late".into());

    // Assert
    assert_eq!(delivered_text, "also add tests");
    assert!(await_acknowledgement(acknowledgement).await.is_ok());
    assert!(matches!(missing, Err(AppServerError::Provider(_))));
    assert!(matches!(after_turn, Err(AppServerError::Provider(_))));
}

#[tokio::test]
async fn replaced_registration_keeps_the_newer_turn_routable() {
    // Arrange
    let registry = TurnSteeringRegistry::default();
    let older = registry.register("session-1");
    let newer = registry.register("session-1");

    // Act
    drop(older);
    let acknowledgement = registry.submit("session-1", "hello".into());

    // Assert
    assert!(acknowledgement.is_ok());
    assert_eq!(newer.inbox().next().await.prompt.text, "hello");
}

#[tokio::test]
async fn unanswered_acknowledgement_counts_as_rejection() {
    // Arrange
    let inbox = TurnSteeringInbox::new();
    let acknowledgement = inbox.submit(TurnPrompt::from("dropped"));

    // Act
    drop(inbox.next().await);
    let result = await_acknowledgement(acknowledgement).await;

    // Assert
    assert_eq!(
        result.expect_err("dropped reply").to_string(),
        turn_not_steerable_error().to_string()
    );
}

#[tokio::test]
async fn accepted_prompts_follow_the_prompt_of_a_restarted_attempt() {
    // Arrange
    let inbox = TurnSteeringInbox::new();
    let _first = inbox.submit("also add tests".into()).expect("open inbox");
    let _second = inbox.submit("rejected".into()).expect("open inbox");
    let _third = inbox.submit("update docs".into()).expect("open inbox");

    // Act
    inbox.next().await.acknowledge(Ok(()));
    inbox
        .next()
        .await
        .acknowledge(Err(turn_not_steerable_error()));
    inbox.next().await.acknowledge(Ok(()));
    let restarted = inbox.prompt_with_accepted(&TurnPrompt::from("original"));

    // Assert
    assert_eq!(restarted.text, "original\n\nalso add tests\n\nupdate docs");
}
