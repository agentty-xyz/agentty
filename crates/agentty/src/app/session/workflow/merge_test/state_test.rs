use tokio::sync::mpsc;

use crate::app::{AppEvent, SessionManager};
use crate::domain::agent::AgentModel;
use crate::infra::db::AppRepositories;
use crate::test_support::enable_tracing_log_fallback;

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
            AgentModel::ClaudeSonnet55.as_str(),
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

#[tokio::test]
async fn test_update_session_title_from_commit_message_tolerates_failed_persistence_and_closed_reducer()
 {
    // Arrange
    enable_tracing_log_fallback();
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session(
            "session-id",
            AgentModel::ClaudeSonnet55.as_str(),
            "main",
            "Review",
            project_id,
        )
        .await
        .expect("failed to insert session");
    sqlx::query(
        "CREATE TRIGGER reject_title BEFORE UPDATE OF title ON session BEGIN SELECT RAISE(ABORT, \
         'title failure'); END",
    )
    .execute(&pool)
    .await
    .expect("failing title fixture");
    let app_event_tx = mpsc::unbounded_channel().0;

    // Act
    SessionManager::update_session_title_from_commit_message(
        &database,
        "session-id",
        "Refine session commit message",
        &app_event_tx,
    )
    .await;
    let sessions = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");

    // Assert
    assert_eq!(sessions[0].title, None);
}

#[test]
fn test_emit_stacked_parent_merge_completed_sends_restacks_and_tolerates_closed_reducer() {
    // Arrange
    enable_tracing_log_fallback();
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let closed_app_event_tx = mpsc::unbounded_channel().0;

    // Act
    SessionManager::emit_stacked_parent_merge_completed(&app_event_tx, Vec::new());
    SessionManager::emit_stacked_parent_merge_completed(&app_event_tx, vec!["child".into()]);
    SessionManager::emit_stacked_parent_merge_completed(&closed_app_event_tx, vec!["child".into()]);

    // Assert
    assert_eq!(
        app_event_rx.try_recv().ok(),
        Some(AppEvent::StackedParentMergeCompleted {
            child_session_ids: vec!["child".into()],
        })
    );
    assert!(app_event_rx.try_recv().is_err());
}
