use std::sync::Arc;

use ag_forge as forge;
use ag_git as git;

use super::super::super::StatusTransition;
use super::super::ReplyEligibility;
use super::support::{
    database_with_session, database_with_session_and_pool, load_persisted_session_row,
    session_manager_with_one_session, session_manager_with_sessions, session_with_id,
    test_services, test_services_with_event_receiver, test_session,
};
use crate::app::session::SessionError;
use crate::app::{AppEvent, SessionManager};
use crate::domain::agent::{ReasoningLevel, ResponseStyle, SpeedMode};
use crate::domain::permission::PermissionMode;
use crate::domain::session::{QueuedMessage, SessionHandles, Status};
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::AppRepositories;
use crate::test_support::{enable_tracing_log_fallback, poison_lock};

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
    for metadata_rejected in [false, true] {
        // Arrange
        let session = test_session("", Status::Merged, None, "");
        let session_id = session.id.clone();
        let (database, pool) = database_with_session_and_pool(&session).await;
        if metadata_rejected {
            sqlx::query(
                "CREATE TRIGGER reject_first_message_metadata BEFORE UPDATE OF title, prompt ON \
                 session BEGIN SELECT RAISE(FAIL, 'metadata rejected'); END",
            )
            .execute(&pool)
            .await
            .expect("failure trigger");
        }
        let session_manager = session_manager_with_one_session(session);
        let (services, mut event_rx) = test_services_with_event_receiver(
            &database,
            Arc::new(git::MockGitClient::new()),
            Arc::new(forge::MockReviewRequestClient::new()),
        );
        let handles = SessionHandles::new(Status::Merged);
        let status_transition =
            StatusTransition::from_services(&services, &handles, session_id.clone());
        enable_tracing_log_fallback();

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
        let session_row = load_persisted_session_row(&database).await;
        let live_status = *handles
            .status
            .lock()
            .expect("status lock should be available");

        // Assert: rejected metadata leaves the previous row values in place.
        assert_eq!(live_status, Status::Merged);
        assert_eq!(session_row.status, Status::Merged.to_string());
        if metadata_rejected {
            assert_eq!(session_row.prompt, "");
            assert_eq!(session_row.title, None);
        } else {
            assert_eq!(session_row.prompt, "Initial prompt");
            assert_eq!(session_row.title.as_deref(), Some("Initial title"));
        }
        assert!(event_rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn cancel_session_reports_a_descendant_without_runtime_handles() {
    // Arrange
    let parent = session_with_id("parent", Status::Review);
    let mut child = session_with_id("child", Status::Draft);
    child.is_draft = true;
    child.parent_session_id = Some("parent".into());
    let database = database_with_session(&parent).await;
    let mut session_manager = session_manager_with_sessions(vec![parent, child]);
    session_manager.state.handles_mut().remove("child");
    let services = test_services(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );
    enable_tracing_log_fallback();

    // Act
    let result = session_manager.cancel_session(&services, "parent").await;

    // Assert
    let error = result.expect_err("descendant cancellation failure should be reported");
    assert!(error.to_string().contains("child"), "{error}");
}

#[tokio::test]
async fn cancellation_signal_continues_past_persistence_and_lock_failures() {
    // Arrange
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    pool.close().await;
    let services = test_services(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );
    let handles = SessionHandles::new(Status::InProgress);
    handles
        .queued_messages
        .lock()
        .expect("queue")
        .push_back(QueuedMessage::new(0, "pending".into()));
    poison_lock(Arc::clone(&handles.queued_messages));
    poison_lock(Arc::clone(&handles.cancel_token));
    enable_tracing_log_fallback();

    // Act
    SessionManager::signal_session_cancellation(&services, &handles, "session-id").await;

    // Assert: poisoned state is left untouched rather than partially reset.
    assert_eq!(
        handles
            .queued_messages
            .lock()
            .expect_err("queue stays poisoned")
            .get_ref()
            .len(),
        1
    );
    assert!(
        !handles
            .cancel_token
            .lock()
            .expect_err("token stays poisoned")
            .get_ref()
            .is_cancelled()
    );
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
