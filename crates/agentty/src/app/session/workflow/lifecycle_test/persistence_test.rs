use std::sync::Arc;

use ag_worker::RunClient;
use tokio::sync::{Notify, mpsc};

use super::support::{
    DelayedTitleClient, load_persisted_session_row, mock_title_client, provisional_title_database,
    title_generation_task_input,
};
use crate::app::SessionManager;
use crate::infra::db::AppRepositories;
use crate::test_support::enable_tracing_log_fallback;

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
    enable_tracing_log_fallback();

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
    enable_tracing_log_fallback();

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
    for failure in [
        TitlePersistenceFailure::Repository,
        TitlePersistenceFailure::TitleUpdate,
    ] {
        // Arrange
        let (database, pool) = provisional_title_database("Background context only.").await;
        if failure == TitlePersistenceFailure::TitleUpdate {
            sqlx::query(
                "CREATE TRIGGER reject_generated_title BEFORE UPDATE OF title ON session WHEN \
                 NEW.title = 'Assess project quality' BEGIN SELECT RAISE(FAIL, 'title rejected'); \
                 END",
            )
            .execute(&pool)
            .await
            .expect("failure trigger");
        }
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let release = Arc::new(Notify::new());
        let run_client: Arc<dyn RunClient> = Arc::new(DelayedTitleClient {
            release: Arc::clone(&release),
        });
        let input = title_generation_task_input(
            app_event_tx,
            database.clone(),
            run_client,
            "review the project",
        );
        enable_tracing_log_fallback();
        let title_generation_task = SessionManager::spawn_session_title_generation_task(input)
            .await
            .expect("title generation should start");

        // Act
        if failure == TitlePersistenceFailure::Repository {
            pool.close().await;
        }
        release.notify_one();
        title_generation_task
            .await
            .expect("title generation task should finish");

        // Assert
        assert!(app_event_rx.try_recv().is_err());
        if failure == TitlePersistenceFailure::TitleUpdate {
            assert_eq!(
                load_persisted_session_row(&database).await.title.as_deref(),
                Some("Background context only.")
            );
        }
    }
}

#[tokio::test]
/// Ensures a generated title persists after the app event receiver closes.
async fn test_title_generation_persists_title_after_event_receiver_closes() {
    // Arrange
    let (database, _pool) = provisional_title_database("Background context only.").await;
    let (app_event_tx, app_event_rx) = mpsc::unbounded_channel();
    drop(app_event_rx);
    let input = title_generation_task_input(
        app_event_tx,
        database.clone(),
        mock_title_client("Assess project quality"),
        "review the project",
    );
    enable_tracing_log_fallback();

    // Act
    SessionManager::spawn_session_title_generation_task(input)
        .await
        .expect("title generation should start")
        .await
        .expect("title generation task should finish");

    // Assert
    assert_eq!(
        load_persisted_session_row(&database).await.title.as_deref(),
        Some("Assess project quality")
    );
}

/// Persistence boundary that fails while a generated title is recorded.
#[derive(Clone, Copy, PartialEq)]
enum TitlePersistenceFailure {
    /// The repository closes before the generation context loads.
    Repository,
    /// Only the generated title update is rejected.
    TitleUpdate,
}
