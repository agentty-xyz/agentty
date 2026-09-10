use std::sync::Arc;

use ag_forge as forge;
use ag_git as git;

use super::super::super::StatusTransition;
use super::super::ReplyEligibility;
use super::support::{
    database_with_session, session_manager_with_one_session, test_services,
    test_services_with_event_receiver, test_session,
};
use crate::app::AppEvent;
use crate::app::session::SessionError;
use crate::domain::agent::{ReasoningLevel, ResponseStyle, SpeedMode};
use crate::domain::permission::PermissionMode;
use crate::domain::session::{SessionHandles, Status};
use crate::domain::turn_prompt::TurnPrompt;

#[tokio::test]
async fn cancel_session_accepts_an_already_applied_status_transition() {
    // Arrange
    let session = test_session("Initial prompt", Status::Review, Some("Title"), "");
    let session_id = session.id.clone();
    let database = database_with_session(&session).await;
    let session_manager = session_manager_with_one_session(session);
    let services = test_services(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );
    let handles = session_manager
        .session_handles()
        .get(&session_id)
        .expect("session handles should exist");
    *handles
        .status
        .lock()
        .expect("session status should remain available") = Status::Merged;

    // Act
    let result = session_manager.cancel_session(&services, &session_id).await;

    // Assert
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_persist_initial_reply_metadata_keeps_terminal_status() {
    // Arrange
    let session = test_session("", Status::Merged, None, "");
    let session_id = session.id.clone();
    let database = database_with_session(&session).await;
    let session_manager = session_manager_with_one_session(session);
    let (services, mut event_rx) = test_services_with_event_receiver(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );
    let handles = SessionHandles::new(Status::Merged);
    let status_transition =
        StatusTransition::from_services(&services, &handles, session_id.clone());

    // Act
    session_manager
        .persist_initial_reply_metadata(
            &services,
            &status_transition,
            &session_id,
            "Initial prompt",
            Some("Initial title".to_string()),
        )
        .await;
    let session_row = database
        .sessions()
        .load_sessions()
        .await
        .expect("sessions should load")
        .into_iter()
        .find(|row| session_id == row.id)
        .expect("session row should exist");
    let live_status = *handles
        .status
        .lock()
        .expect("status lock should be available");

    // Assert
    assert_eq!(live_status, Status::Merged);
    assert_eq!(session_row.status, Status::Merged.to_string());
    assert_eq!(session_row.prompt, "Initial prompt");
    assert_eq!(session_row.title.as_deref(), Some("Initial title"));
    assert!(event_rx.try_recv().is_err());
}

#[test]
/// Ensures replying to an in-progress session returns a typed
/// [`SessionError::Workflow`] instead of a raw string.
fn test_prepare_reply_context_returns_workflow_error_when_status_blocks_reply() {
    // Arrange
    let session = test_session("Initial prompt", Status::InProgress, Some("Title"), "");
    let mut session_manager = session_manager_with_one_session(session);
    let prompt = TurnPrompt::from_text("Another prompt".to_string());

    // Act
    let result = session_manager.prepare_reply_context(
        "session-id",
        &prompt,
        false,
        ReplyEligibility::Standard,
    );

    // Assert
    let error = result.expect_err("in-progress session should block reply");
    assert!(
        matches!(error, SessionError::Workflow(_)),
        "expected SessionError::Workflow, got: {error:?}"
    );
}

#[tokio::test]
/// Ensures `set_session_reasoning_level()` persists the level and
/// emits the matching reducer event.
async fn test_set_session_reasoning_level_persists_level_and_emits_event() {
    // Arrange
    let session = test_session("Prompt", Status::Review, Some("Title"), "");
    let database = database_with_session(&session).await;
    let mut session_manager = session_manager_with_one_session(session);
    let (services, mut event_rx) = test_services_with_event_receiver(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    // Act
    session_manager
        .set_session_reasoning_level(&services, "session-id", ReasoningLevel::High)
        .await
        .expect("reasoning level update should succeed");
    let persisted_reasoning_level = database
        .sessions()
        .load_session_reasoning_level("session-id")
        .await
        .expect("reasoning level should load");
    let emitted_event = event_rx
        .try_recv()
        .expect("expected reasoning update event");

    // Assert
    assert_eq!(persisted_reasoning_level, ReasoningLevel::High);
    assert_eq!(
        emitted_event,
        AppEvent::SessionReasoningLevelUpdated {
            reasoning_level: ReasoningLevel::High,
            session_id: "session-id".into(),
        }
    );
    assert!(event_rx.try_recv().is_err());
}

#[tokio::test]
/// Ensures `set_session_response_style()` persists the preference and
/// emits the matching reducer event.
async fn test_set_session_response_style_persists_style_and_emits_event() {
    // Arrange
    let session = test_session("Prompt", Status::Review, Some("Title"), "");
    let database = database_with_session(&session).await;
    let mut session_manager = session_manager_with_one_session(session);
    let (services, mut event_rx) = test_services_with_event_receiver(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    // Act
    session_manager
        .set_session_response_style(&services, "session-id", ResponseStyle::Detailed)
        .await
        .expect("response style update should succeed");
    let persisted_response_style = database
        .sessions()
        .load_session_response_style("session-id")
        .await
        .expect("response style should load");
    let emitted_event = event_rx.try_recv().expect("expected style update event");

    // Assert
    assert_eq!(persisted_response_style, ResponseStyle::Detailed);
    assert_eq!(
        emitted_event,
        AppEvent::SessionResponseStyleUpdated {
            response_style: ResponseStyle::Detailed,
            session_id: "session-id".into(),
        }
    );
    assert!(event_rx.try_recv().is_err());
}

#[tokio::test]
/// Ensures `set_session_permission_mode()` persists the mode and emits
/// the matching reducer event.
async fn test_set_session_permission_mode_persists_mode_and_emits_event() {
    // Arrange
    let session = test_session("Prompt", Status::Review, Some("Title"), "");
    let database = database_with_session(&session).await;
    let mut session_manager = session_manager_with_one_session(session);
    let (services, mut event_rx) = test_services_with_event_receiver(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    // Act
    session_manager
        .set_session_permission_mode(&services, "session-id", PermissionMode::ReadOnly)
        .await
        .expect("permission mode update should succeed");
    let persisted_permission_mode = database
        .sessions()
        .load_session_permission_mode("session-id")
        .await
        .expect("permission mode should load");
    let emitted_event = event_rx
        .try_recv()
        .expect("expected permission update event");

    // Assert
    assert_eq!(persisted_permission_mode, PermissionMode::ReadOnly);
    assert_eq!(
        emitted_event,
        AppEvent::SessionPermissionModeUpdated {
            permission_mode: PermissionMode::ReadOnly,
            session_id: "session-id".into(),
        }
    );
    assert!(event_rx.try_recv().is_err());
}

#[tokio::test]
/// Ensures `set_session_speed_mode()` persists the preference and emits
/// the matching reducer event.
async fn test_set_session_speed_mode_persists_mode_and_emits_event() {
    // Arrange
    let session = test_session("Prompt", Status::Review, Some("Title"), "");
    let database = database_with_session(&session).await;
    let mut session_manager = session_manager_with_one_session(session);
    let (services, mut event_rx) = test_services_with_event_receiver(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    // Act
    session_manager
        .set_session_speed_mode(&services, "session-id", SpeedMode::Fast)
        .await
        .expect("speed mode update should succeed");
    let persisted_speed_mode = database
        .sessions()
        .load_session_speed_mode("session-id")
        .await
        .expect("speed mode should load");
    let emitted_event = event_rx.try_recv().expect("expected speed update event");

    // Assert
    assert_eq!(persisted_speed_mode, SpeedMode::Fast);
    assert_eq!(
        emitted_event,
        AppEvent::SessionSpeedModeUpdated {
            session_id: "session-id".into(),
            speed_mode: SpeedMode::Fast,
        }
    );
    assert!(event_rx.try_recv().is_err());
}
