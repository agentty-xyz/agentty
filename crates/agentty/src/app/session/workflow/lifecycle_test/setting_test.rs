use std::sync::Arc;

use ag_forge as forge;
use ag_git as git;

use super::support::{
    database_with_session, session_manager_with_one_session, test_services,
    test_services_with_event_receiver, test_session,
};
use crate::app::AppEvent;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session::Status;

#[tokio::test]
async fn set_session_model_persists_new_model_and_clears_conversation_state() {
    // Arrange
    let mut session = test_session("Prompt", Status::Review, Some("Title"), "");
    session.agent = AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5);
    let database = database_with_session(&session).await;
    database
        .sessions()
        .update_session_provider_conversation_id("session-id", Some("provider-conv".to_string()))
        .await
        .expect("seed provider conversation id");
    database
        .sessions()
        .update_session_instruction_conversation_id(
            "session-id",
            Some("instruction-conv".to_string()),
        )
        .await
        .expect("seed instruction conversation id");
    let mut session_manager = session_manager_with_one_session(session);
    let (services, mut event_rx) = test_services_with_event_receiver(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    // Act
    session_manager
        .set_session_model(
            &services,
            "session-id",
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
        )
        .await
        .expect("set session model should succeed");
    let persisted_model = database
        .sessions()
        .load_sessions()
        .await
        .expect("load sessions should succeed")
        .into_iter()
        .find(|row| row.id == "session-id")
        .expect("session row should exist")
        .model;
    let cleared_provider = database
        .sessions()
        .get_session_provider_conversation_id("session-id")
        .await
        .expect("provider id load should succeed");
    let cleared_instruction = database
        .sessions()
        .get_session_instruction_conversation_id("session-id")
        .await
        .expect("instruction id load should succeed");
    let emitted_event = event_rx.try_recv().expect("model event expected");

    // Assert
    assert_eq!(persisted_model, AgentModel::Gpt56Sol.as_str());
    assert!(cleared_provider.is_none());
    assert!(cleared_instruction.is_none());
    assert_eq!(
        emitted_event,
        AppEvent::SessionModelUpdated {
            session_id: "session-id".into(),
            session_agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
        }
    );
    assert!(session_manager.should_replay_history("session-id"));
}

#[tokio::test]
async fn set_session_model_keeps_conversation_state_when_model_does_not_change() {
    // Arrange
    let mut session = test_session("Prompt", Status::InProgress, Some("Title"), "");
    session.agent = AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5);
    let database = database_with_session(&session).await;
    database
        .sessions()
        .update_session_provider_conversation_id("session-id", Some("provider-conv".to_string()))
        .await
        .expect("seed provider conversation id");
    let mut session_manager = session_manager_with_one_session(session);
    let (services, mut event_rx) = test_services_with_event_receiver(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    // Act
    session_manager
        .set_session_model(
            &services,
            "session-id",
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
        )
        .await
        .expect("set session model should succeed");
    let preserved_provider = database
        .sessions()
        .get_session_provider_conversation_id("session-id")
        .await
        .expect("provider id load should succeed");
    let emitted_event = event_rx.try_recv().expect("model event expected");

    // Assert
    assert_eq!(preserved_provider.as_deref(), Some("provider-conv"));
    assert_eq!(
        emitted_event,
        AppEvent::SessionModelUpdated {
            session_id: "session-id".into(),
            session_agent: AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
        }
    );
    assert!(!session_manager.should_replay_history("session-id"));
}

#[tokio::test]
async fn set_session_model_returns_error_for_missing_session() {
    // Arrange
    let session = test_session("Prompt", Status::Review, Some("Title"), "");
    let database = database_with_session(&session).await;
    let mut session_manager = session_manager_with_one_session(session);
    let services = test_services(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    // Act
    let result = session_manager
        .set_session_model(
            &services,
            "missing",
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
        )
        .await;

    // Assert
    assert!(
        result.is_err(),
        "missing session should return SessionError"
    );
}
