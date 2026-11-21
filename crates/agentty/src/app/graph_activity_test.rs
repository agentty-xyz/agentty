//! Graph activity lease ownership of session status unit tests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ag_session::test_support::graph_proposal_state;
use tokio_util::sync::CancellationToken;

use super::{GRAPH_ACTIVITY_OPERATION_KIND, GraphActivityRegistry, StatusChange};
use crate::app::SessionManager;
use crate::app::execution_graph::tests::seed_session;
use crate::app::graph_proposal::ProposalGeneration;
use crate::app::graph_reply_decision::ReplyDecision;
use crate::app::service::AppServices;
use crate::app::session::StatusTransition;
use crate::domain::session::{SessionId, Status};
use crate::test_support::new_test_app;

#[test]
fn first_lease_holds_a_review_session_in_progress_until_the_last_lease_ends() {
    // Arrange
    let registry = GraphActivityRegistry::default();
    let session_id = SessionId::from("sess");
    let status = Mutex::new(Status::Review);

    // Act
    let first_claimed = registry.acquire(&session_id, &status);
    let second_claimed = registry.acquire(&session_id, &status);
    let first_restored = registry.release(&session_id, &status);
    let status_between_releases = *status.lock().expect("status");
    let last_restored = registry.release(&session_id, &status);

    // Assert
    assert!(first_claimed);
    assert!(!second_claimed);
    assert!(!first_restored);
    assert_eq!(status_between_releases, Status::InProgress);
    assert!(last_restored);
    assert_eq!(*status.lock().expect("status"), Status::Review);
}

#[test]
fn lease_on_a_prepared_draft_shows_in_progress_and_ends_in_review() {
    // Arrange
    let registry = GraphActivityRegistry::default();
    let session_id = SessionId::from("sess");
    let status = Mutex::new(Status::Draft);

    // Act
    let claimed = registry.acquire(&session_id, &status);
    let status_during_lease = *status.lock().expect("status");
    let restored = registry.release(&session_id, &status);

    // Assert
    assert!(claimed);
    assert_eq!(status_during_lease, Status::InProgress);
    assert!(restored);
    assert_eq!(*status.lock().expect("status"), Status::Review);
}

#[test]
fn lease_on_a_busy_or_reviewing_session_keeps_its_status() {
    for kept in [
        Status::InProgress,
        Status::Rebasing,
        Status::AgentReview,
        Status::Watching,
        Status::Question,
    ] {
        // Arrange
        let registry = GraphActivityRegistry::default();
        let session_id = SessionId::from("sess");
        let status = Mutex::new(kept);

        // Act
        let claimed = registry.acquire(&session_id, &status);
        let restored = registry.release(&session_id, &status);

        // Assert
        assert!(!claimed, "{kept}");
        assert!(!restored, "{kept}");
        assert_eq!(*status.lock().expect("status"), kept);
    }
}

#[test]
fn rebase_ending_during_a_lease_returns_the_session_to_the_lease() {
    // Arrange
    let registry = GraphActivityRegistry::default();
    let session_id = SessionId::from("sess");
    let status = Mutex::new(Status::Review);
    registry.acquire(&session_id, &status);

    // Act
    let rebase_start = registry.transition(&session_id, &status, Status::Rebasing);
    let rebase_end = registry.transition(&session_id, &status, Status::Review);
    let status_after_rebase = *status.lock().expect("status");
    let restored = registry.release(&session_id, &status);

    // Assert
    assert_eq!(rebase_start, StatusChange::Applied);
    assert_eq!(rebase_end, StatusChange::Held);
    assert_eq!(status_after_rebase, Status::InProgress);
    assert!(restored);
    assert_eq!(*status.lock().expect("status"), Status::Review);
}

#[test]
fn turn_ending_during_a_lease_defers_review_to_the_last_lease() {
    // Arrange
    let registry = GraphActivityRegistry::default();
    let session_id = SessionId::from("sess");
    let status = Mutex::new(Status::InProgress);
    registry.acquire(&session_id, &status);

    // Act
    let turn_end = registry.transition(&session_id, &status, Status::Review);
    let status_after_turn = *status.lock().expect("status");
    let restored = registry.release(&session_id, &status);

    // Assert
    assert_eq!(turn_end, StatusChange::Deferred);
    assert_eq!(status_after_turn, Status::InProgress);
    assert!(restored);
    assert_eq!(*status.lock().expect("status"), Status::Review);
}

#[test]
fn turn_starting_during_a_lease_keeps_running_after_the_lease_ends() {
    // Arrange
    let registry = GraphActivityRegistry::default();
    let session_id = SessionId::from("sess");
    let status = Mutex::new(Status::Review);
    registry.acquire(&session_id, &status);

    // Act
    let turn_start = registry.transition(&session_id, &status, Status::InProgress);
    let restored = registry.release(&session_id, &status);
    let status_after_lease = *status.lock().expect("status");
    let turn_end = registry.transition(&session_id, &status, Status::Review);

    // Assert
    assert_eq!(turn_start, StatusChange::Applied);
    assert!(!restored);
    assert_eq!(status_after_lease, Status::InProgress);
    assert_eq!(turn_end, StatusChange::Applied);
    assert_eq!(*status.lock().expect("status"), Status::Review);
}

#[test]
fn transitions_without_leases_follow_the_lifecycle() {
    // Arrange
    let registry = GraphActivityRegistry::default();
    let session_id = SessionId::from("sess");
    let status = Mutex::new(Status::Draft);

    // Act
    let to_review = registry.transition(&session_id, &status, Status::Review);
    let to_in_progress = registry.transition(&session_id, &status, Status::InProgress);

    // Assert
    assert_eq!(to_review, StatusChange::Rejected);
    assert_eq!(to_in_progress, StatusChange::Applied);
    assert_eq!(*status.lock().expect("status"), Status::InProgress);
}

#[test]
fn release_leaves_a_status_the_lease_no_longer_owns() {
    // Arrange
    let registry = GraphActivityRegistry::default();
    let session_id = SessionId::from("sess");
    let status = Mutex::new(Status::Review);
    let without_lease = registry.release(&session_id, &status);
    registry.acquire(&session_id, &status);
    // Stopping the session publishes `Review` directly.
    *status.lock().expect("status") = Status::Review;

    // Act
    let restored = registry.release(&session_id, &status);

    // Assert
    assert!(!without_lease);
    assert!(!restored);
    assert_eq!(*status.lock().expect("status"), Status::Review);
}

#[tokio::test]
async fn lease_shows_in_progress_at_once_and_persists_it_with_recovery_until_it_finishes() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let session_id = SessionId::from("sess");
    let status = handle_status(&app.sessions, &session_id);
    let wake = app.services.graph_wake.subscribe();

    // Act
    let activity = app
        .services
        .begin_graph_activity(&session_id, Some(Arc::clone(&status)));
    let status_before_record = *status.lock().expect("status");
    let stored_before_record = stored_status(&app.services).await;
    activity.record(&app.services).await;
    let status_while_held = stored_status(&app.services).await;
    let operations_while_held = unfinished_kinds(&app.services).await;
    let woke_while_held = wake.has_changed().expect("wake");
    activity.finish(&app.services).await;

    // Assert
    assert_eq!(status_before_record, Status::InProgress);
    assert_eq!(stored_before_record, Status::Review.to_string());
    assert_eq!(status_while_held, Status::InProgress.to_string());
    assert_eq!(operations_while_held, [GRAPH_ACTIVITY_OPERATION_KIND]);
    assert!(!woke_while_held);
    assert!(wake.has_changed().expect("wake"), "blocked drivers retry");
    assert_eq!(
        stored_status(&app.services).await,
        Status::Review.to_string()
    );
    assert_eq!(*status.lock().expect("status"), Status::Review);
    assert_eq!(unfinished_kinds(&app.services).await, Vec::<String>::new());
}

#[tokio::test]
async fn lease_for_an_unloaded_session_only_records_recovery() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let session_id = SessionId::from("sess");

    // Act
    let activity = app.services.begin_graph_activity(&session_id, None);
    activity.record(&app.services).await;
    let operations_while_held = unfinished_kinds(&app.services).await;
    activity.finish(&app.services).await;

    // Assert
    assert_eq!(operations_while_held, [GRAPH_ACTIVITY_OPERATION_KIND]);
    assert_eq!(
        stored_status(&app.services).await,
        Status::Review.to_string()
    );
    assert_eq!(unfinished_kinds(&app.services).await, Vec::<String>::new());
}

#[tokio::test]
async fn turn_review_transition_waits_for_the_graph_lease() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let session_id = SessionId::from("sess");
    let handles = app
        .sessions
        .session_handles_or_err(&session_id)
        .expect("handles");
    let status_transition = StatusTransition::from_services(&app.services, handles, "sess");
    let status = Arc::clone(&handles.status);
    let activity = app
        .services
        .begin_graph_activity(&session_id, Some(Arc::clone(&status)));
    activity.record(&app.services).await;

    // Act
    let turn_started = status_transition.apply(Status::InProgress).await;
    let turn_ended = status_transition.apply(Status::Review).await;
    let status_before_finish = *status.lock().expect("status");
    activity.finish(&app.services).await;

    // Assert
    assert!(turn_started);
    assert!(turn_ended);
    assert_eq!(status_before_finish, Status::InProgress);
    assert_eq!(*status.lock().expect("status"), Status::Review);
    assert_eq!(
        stored_status(&app.services).await,
        Status::Review.to_string()
    );
}

#[tokio::test]
async fn rebase_review_transition_publishes_the_held_lease_status() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let session_id = SessionId::from("sess");
    let handles = app
        .sessions
        .session_handles_or_err(&session_id)
        .expect("handles");
    let status_transition = StatusTransition::from_services(&app.services, handles, "sess");
    let status = Arc::clone(&handles.status);
    let activity = app
        .services
        .begin_graph_activity(&session_id, Some(Arc::clone(&status)));
    activity.record(&app.services).await;

    // Act
    let rebase_started = status_transition.apply(Status::Rebasing).await;
    let rebase_ended = status_transition.apply(Status::Review).await;
    let stored_before_finish = stored_status(&app.services).await;
    activity.finish(&app.services).await;

    // Assert
    assert!(rebase_started);
    assert!(rebase_ended);
    assert_eq!(stored_before_finish, Status::InProgress.to_string());
    assert_eq!(*status.lock().expect("status"), Status::Review);
    assert_eq!(
        stored_status(&app.services).await,
        Status::Review.to_string()
    );
}

#[tokio::test]
async fn lease_end_persists_a_turn_status_that_overtook_it() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let session_id = SessionId::from("sess");
    let status = handle_status(&app.sessions, &session_id);
    let activity = app
        .services
        .begin_graph_activity(&session_id, Some(Arc::clone(&status)));
    activity.record(&app.services).await;
    let registry = app.services.graph_activity.clone();
    let publication = registry.lock_publication().await;
    // A turn that starts after the lease restores `Review` in memory, but
    // whose write reaches the database before the lease's write.
    let turn_start = async {
        let turn_started = registry.transition(&session_id, &status, Status::InProgress);
        app.services
            .db()
            .sessions()
            .update_session_status_with_timing_at("sess", &Status::InProgress.to_string(), 0)
            .await
            .expect("turn status");
        drop(publication);

        turn_started
    };

    // Act
    let ((), turn_started) = tokio::join!(activity.finish(&app.services), turn_start);

    // Assert
    assert_eq!(turn_started, StatusChange::Applied);
    assert_eq!(*status.lock().expect("status"), Status::InProgress);
    assert_eq!(
        stored_status(&app.services).await,
        Status::InProgress.to_string()
    );
}

#[tokio::test]
async fn stopping_a_session_stops_only_its_graph_work() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    app.services
        .db()
        .graph_proposals()
        .create(&graph_proposal_state("preview", "sess"))
        .await
        .expect("proposal");
    let (stopped, kept) = (CancellationToken::new(), CancellationToken::new());
    *app.services
        .proposal_generations
        .lock()
        .expect("generations") = HashMap::from([
        (
            "preview".to_string(),
            ProposalGeneration {
                cancellation: stopped.clone(),
                session_id: SessionId::from("sess"),
                version: 1,
            },
        ),
        (
            "other-preview".to_string(),
            ProposalGeneration {
                cancellation: kept.clone(),
                session_id: SessionId::from("other"),
                version: 1,
            },
        ),
    ]);
    let (stopped_reply, kept_reply) = (CancellationToken::new(), CancellationToken::new());
    *app.services
        .output_reply_decisions
        .lock()
        .expect("decisions") = HashMap::from([
        (
            "result".to_string(),
            ReplyDecision {
                cancellation: stopped_reply.clone(),
                is_abandoned: Arc::default(),
                session_id: SessionId::from("sess"),
            },
        ),
        (
            "other-result".to_string(),
            ReplyDecision {
                cancellation: kept_reply.clone(),
                is_abandoned: Arc::default(),
                session_id: SessionId::from("other"),
            },
        ),
    ]);

    // Act
    app.services.stop_graph_activity("sess").await;

    // Assert
    assert!(stopped.is_cancelled());
    assert!(!kept.is_cancelled());
    assert!(stopped_reply.is_cancelled());
    assert!(!kept_reply.is_cancelled());
    let generations = app
        .services
        .proposal_generations
        .lock()
        .expect("generations")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(generations, ["other-preview"]);
    assert!(
        app.services
            .db()
            .graph_proposals()
            .get("preview", None)
            .await
            .expect("query")
            .expect("proposal")
            .version
            .is_cancelled
    );
}

fn handle_status(sessions: &SessionManager, session_id: &SessionId) -> Arc<Mutex<Status>> {
    Arc::clone(
        &sessions
            .session_handles_or_err(session_id)
            .expect("handles")
            .status,
    )
}

async fn stored_status(services: &AppServices) -> String {
    services
        .db()
        .sessions()
        .load_session("sess")
        .await
        .expect("session row")
        .expect("session exists")
        .status
}

async fn unfinished_kinds(services: &AppServices) -> Vec<String> {
    services
        .db()
        .operations()
        .load_unfinished_session_operations()
        .await
        .expect("operations")
        .into_iter()
        .map(|operation| operation.kind)
        .collect()
}
