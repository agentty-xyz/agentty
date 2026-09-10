use std::sync::Arc;

use ag_forge as forge;
use ag_git as git;

use super::super::{
    BuildSessionCommandInput, ReplyEligibility, ReplyEnqueueOptions, ReplyEnqueueOutcome,
};
use super::support::{
    database_with_session, session_manager_with_one_session, test_services,
    test_services_with_event_receiver, test_session,
};
use crate::app::{AppEvent, SessionManager};
use crate::domain::session::Status;
use crate::domain::session_message::SessionMessageKind;
use crate::domain::turn_prompt::TurnPrompt;

#[test]
/// Ensures follow-up replies keep the existing title unchanged.
fn test_prepare_reply_context_follow_up_keeps_existing_title() {
    // Arrange
    let session = test_session(
        "Initial prompt",
        Status::Review,
        Some("Initial prompt"),
        "existing output",
    );
    let mut session_manager = session_manager_with_one_session(session);
    let prompt = TurnPrompt::from_text("Follow-up prompt".to_string());

    // Act
    let context = session_manager
        .prepare_reply_context("session-id", &prompt, false, ReplyEligibility::Standard)
        .expect("reply context should be available");

    // Assert
    assert_eq!(context.0, None);
    assert!(!context.1);
    assert_eq!(context.2, "session-id");
    assert_eq!(context.3, None);
    assert_eq!(session_manager.sessions()[0].prompt, "Initial prompt");
    assert_eq!(
        session_manager.sessions()[0].title,
        Some("Initial prompt".to_string())
    );
}

#[tokio::test]
/// Verifies `enqueue_message()` emits a single targeted
/// [`AppEvent::SessionUpdated`] for the touched session and never falls
/// back to [`AppEvent::RefreshSessions`]. The targeted event lets the
/// reducer re-sync only the affected snapshot from handles instead of
/// paying for a full DB-backed reload, which is the contract that makes
/// queued chat rows appear without a perceptible delay.
async fn test_enqueue_message_emits_session_updated_event_only() {
    // Arrange
    let session = test_session("Prompt", Status::InProgress, Some("Title"), "");
    let database = database_with_session(&session).await;
    let mut session_manager = session_manager_with_one_session(session);
    let (services, mut event_rx) = test_services_with_event_receiver(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    // Act
    session_manager
        .enqueue_message(&services, "session-id", "queued reply")
        .expect("enqueue_message should succeed for InProgress session");

    // Assert
    let emitted_event = event_rx
        .try_recv()
        .expect("expected SessionUpdated event from enqueue_message");
    assert!(
        matches!(
            &emitted_event,
            AppEvent::SessionUpdated { session_id, .. }
                if AsRef::<str>::as_ref(session_id) == "session-id"
        ),
        "enqueue_message must emit SessionUpdated, got {emitted_event:?}"
    );
    assert!(
        event_rx.try_recv().is_err(),
        "enqueue_message must not emit additional events (especially not RefreshSessions) so the \
         reducer skips the full DB-backed reload"
    );
}

#[tokio::test]
/// Ensures a normal reply enqueue failure remains visible in the durable
/// workflow transcript.
async fn test_enqueue_reply_command_reports_worker_failure_in_transcript() {
    // Arrange
    let session = test_session("Initial prompt", Status::Review, Some("Title"), "");
    let session_agent = session.agent;
    let database = database_with_session(&session).await;
    let mut session_manager = session_manager_with_one_session(session);
    let services = test_services(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );
    let transcript = Arc::clone(
        &session_manager
            .session_handles_or_err("session-id")
            .expect("session handles should exist")
            .transcript,
    );
    let prompt = TurnPrompt::from_text("Continue".to_string());
    let command = SessionManager::build_session_command(BuildSessionCommandInput {
        is_first_message: false,
        operation_id: None,
        prompt: prompt.clone(),
        published_upstream_ref: None,
        replay_transcript: None,
        review_comment_thread_ids: Vec::new(),
        session_agent,
    });

    // Act
    let outcome = session_manager
        .enqueue_reply_command(
            &services,
            &transcript,
            "session-id",
            &prompt,
            command,
            ReplyEnqueueOptions {
                idempotent: false,
                report_failure_in_transcript: true,
                requires_existing_worker: true,
            },
        )
        .await;
    let messages = database
        .sessions()
        .load_session_messages("session-id")
        .await
        .expect("failed to load workflow transcript");

    // Assert
    assert!(outcome == ReplyEnqueueOutcome::Failed);
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].kind,
        SessionMessageKind::WorkflowNotice.to_string()
    );
    assert!(
        messages[0]
            .content
            .contains("active session worker is unavailable")
    );
}
