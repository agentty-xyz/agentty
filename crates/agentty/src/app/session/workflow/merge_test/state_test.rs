use tokio::sync::mpsc;

use crate::app::{AppEvent, SessionManager};
use crate::domain::agent::AgentModel;
use crate::infra::db::AppRepositories;

#[tokio::test]
async fn test_update_session_title_from_commit_message_persists_title() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session(
            "session-id",
            AgentModel::ClaudeSonnet5.as_str(),
            "main",
            "Review",
            project_id,
        )
        .await
        .expect("failed to insert session");
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let commit_message = "Refine session commit message\n\n- Keep title in sync";

    // Act
    SessionManager::update_session_title_from_commit_message(
        &database,
        "session-id",
        commit_message,
        &app_event_tx,
    )
    .await;
    let sessions = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");

    // Assert
    assert_eq!(
        sessions[0].title.as_deref(),
        Some("Refine session commit message")
    );
    assert_eq!(
        app_event_rx.try_recv().ok(),
        Some(AppEvent::RefreshSessions)
    );
}
