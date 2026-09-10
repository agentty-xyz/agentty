use std::sync::Arc;

use ag_agent::OneShotClient;
use tokio::sync::{Notify, mpsc};

use super::support::{DelayedTitleClient, provisional_title_database, title_generation_task_input};
use crate::app::SessionManager;
use crate::infra::db::AppRepositories;

#[tokio::test]
/// Ensures title generation loads the persisted original goal, current
/// title, and latest request into one stable context snapshot.
async fn test_load_session_title_generation_context_returns_persisted_context() {
    // Arrange
    let (database, _pool) = provisional_title_database("Stabilize session titles").await;
    // Act
    let context = SessionManager::load_session_title_generation_context(
        &database,
        "session-id",
        "Also reject punctuation-only copies".to_string(),
    )
    .await
    .expect("title context should load");

    // Assert
    assert_eq!(context.current_title, "Stabilize session titles");
    assert_eq!(
        context.latest_request,
        "Also reject punctuation-only copies"
    );
    assert_eq!(context.original_request, "Stabilize session titles");
}

#[tokio::test]
/// Ensures a deleted session cannot launch a context-free title request.
async fn test_load_session_title_generation_context_returns_none_for_missing_session() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");

    // Act
    let context = SessionManager::load_session_title_generation_context(
        &database,
        "missing-session",
        "Latest request".to_string(),
    )
    .await;

    // Assert
    assert!(context.is_none());
}

#[tokio::test]
/// Ensures a repository failure cannot launch a context-free title
/// request.
async fn test_load_session_title_generation_context_returns_none_for_repository_failure() {
    // Arrange
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    pool.close().await;

    // Act
    let context = SessionManager::load_session_title_generation_context(
        &database,
        "session-id",
        "Latest request".to_string(),
    )
    .await;

    // Assert
    assert!(context.is_none());
}

#[tokio::test]
/// Ensures a persistence failure after generation finishes cleanly without
/// publishing a refresh event.
async fn test_title_generation_handles_persistence_failure() {
    // Arrange
    let (database, pool) = provisional_title_database("Background context only.").await;
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    let one_shot_client: Arc<dyn OneShotClient> = Arc::new(DelayedTitleClient {
        release: Arc::clone(&release),
    });
    let input = title_generation_task_input(
        app_event_tx,
        database,
        one_shot_client,
        "review the project",
    );
    let title_generation_task = SessionManager::spawn_session_title_generation_task(input)
        .await
        .expect("title generation should start");

    // Act
    pool.close().await;
    release.notify_one();
    title_generation_task
        .await
        .expect("title generation task should finish");

    // Assert
    assert!(app_event_rx.try_recv().is_err());
}
