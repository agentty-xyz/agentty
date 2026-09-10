use ag_session::SessionMessageKind;

use crate::connection::open_in_memory_pool;
use crate::session_message::APPEND_SESSION_MESSAGE;
use crate::{AppRepositories, DbError};

#[tokio::test]
async fn blank_message_returns_without_persistence() {
    // Arrange
    let pool = open_in_memory_pool(1)
        .await
        .expect("failed to open in-memory db");
    let repositories = AppRepositories::from_pool(pool);

    // Act
    let result = repositories
        .sessions()
        .append_session_message("missing-session", SessionMessageKind::UserPrompt, " \n ")
        .await;

    // Assert
    assert!(result.is_ok());
}

#[tokio::test]
async fn append_after_session_deletion_does_not_create_orphan_messages() {
    // Arrange
    let repositories = AppRepositories::in_memory().await.expect("database");

    // Act
    repositories
        .sessions()
        .append_session_message(
            "deleted-session",
            SessionMessageKind::UserPrompt,
            "late prompt",
        )
        .await
        .expect("missing session is ignored");
    let messages = repositories
        .sessions()
        .load_session_messages("deleted-session")
        .await
        .expect("messages");

    // Assert
    assert_eq!(messages.len(), 0);
}

#[tokio::test]
async fn append_failure_reports_semantic_operation_context() {
    // Arrange
    let pool = open_in_memory_pool(1)
        .await
        .expect("failed to open in-memory db");
    let repositories = AppRepositories::from_pool(pool.clone());
    let project_id = repositories
        .projects()
        .upsert_project("/tmp/message-context", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    repositories
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    sqlx::query("DROP TABLE session_message")
        .execute(&pool)
        .await
        .expect("failed to drop message table");

    // Act
    let error = repositories
        .sessions()
        .append_session_message("session-a", SessionMessageKind::UserPrompt, "Persist this")
        .await
        .expect_err("append should fail");

    // Assert
    assert!(matches!(
        error,
        DbError::QueryContext {
            operation: APPEND_SESSION_MESSAGE,
            ..
        }
    ));
}
