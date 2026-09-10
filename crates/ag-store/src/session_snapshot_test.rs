use crate::connection::open_in_memory_pool;
use crate::session::ForkSessionSnapshot;
use crate::session_snapshot::FORK_SESSION_SNAPSHOT;
use crate::{AppRepositories, DbError};

#[tokio::test]
async fn missing_snapshot_source_reports_semantic_operation_context() {
    // Arrange
    let pool = open_in_memory_pool(1)
        .await
        .expect("failed to open in-memory db");
    let repositories = AppRepositories::from_pool(pool);

    // Act
    let error = repositories
        .sessions()
        .fork_session_snapshot(ForkSessionSnapshot {
            new_session_id: "fork-session",
            source_session_id: "missing-session",
            status: "Draft",
        })
        .await
        .expect_err("fork should fail");

    // Assert
    assert!(matches!(
        error,
        DbError::QueryContext {
            operation: FORK_SESSION_SNAPSHOT,
            source: sqlx::Error::RowNotFound,
        }
    ));
}
