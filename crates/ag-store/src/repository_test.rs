use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use ag_session::{SessionDiffState, SessionMessageKind, SessionStats};
use ag_worker::MockOperationRepository;
use sqlx::SqlitePool;

use crate::connection::{Database, MIGRATOR, open_in_memory_pool};
use crate::repository::AppRepositories;
use crate::timestamp::TimestampSource;

/// Timestamp fixture used to verify persistence composition
/// deterministically.
struct AdvancingTimestampSource {
    next_timestamp: AtomicI64,
}

impl TimestampSource for AdvancingTimestampSource {
    fn now_timestamp_seconds(&self) -> i64 {
        self.next_timestamp.fetch_add(1, Ordering::Relaxed)
    }
}

/// Timestamp fixture used to expose accidental fallbacks to `SQLite` time.
struct FixedTimestampSource;

impl TimestampSource for FixedTimestampSource {
    fn now_timestamp_seconds(&self) -> i64 {
        456
    }
}

#[tokio::test]
async fn injected_clock_drives_all_repository_timestamps() {
    // Arrange
    let pool = open_in_memory_pool(1)
        .await
        .expect("failed to open in-memory db");
    let timestamp_source = Arc::new(AdvancingTimestampSource {
        next_timestamp: AtomicI64::new(120),
    });
    let repositories =
        AppRepositories::from_pool_and_timestamp_source(pool.clone(), timestamp_source);

    // Act
    let project_id = repositories
        .projects()
        .upsert_project("/tmp/injected-clock", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    repositories
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    repositories
        .sessions()
        .append_session_message("session-a", SessionMessageKind::UserPrompt, "Persist this")
        .await
        .expect("failed to append message");
    repositories
        .operations()
        .insert_session_operation("operation-a", "session-a", "reply")
        .await
        .expect("failed to insert operation");
    repositories
        .usage()
        .upsert_session_usage(
            "session-a",
            "gpt-5.6-sol",
            &SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: SessionDiffState::Unknown,
                input_tokens: 1,
                output_tokens: 2,
            },
        )
        .await
        .expect("failed to insert usage");
    let timestamps = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64, i64)>(
        r"
SELECT project.created_at,
       project.updated_at,
       session.created_at,
       session.updated_at,
       session_message.created_at,
       session_operation.queued_at,
       session_usage.created_at
FROM project
INNER JOIN session ON session.project_id = project.id
INNER JOIN session_message ON session_message.session_id = session.id
INNER JOIN session_operation ON session_operation.session_id = session.id
INNER JOIN session_usage ON session_usage.session_id = session.id
",
    )
    .fetch_one(&pool)
    .await
    .expect("failed to load timestamps");

    // Assert
    assert_eq!(timestamps, (120, 120, 121, 122, 122, 123, 124));
}

#[tokio::test]
async fn fixed_clock_drives_session_metadata_status_and_usage_writes() {
    // Arrange
    let pool = open_in_memory_pool(1)
        .await
        .expect("failed to open in-memory db");
    let timestamp_source = Arc::new(FixedTimestampSource);
    let repositories =
        AppRepositories::from_pool_and_timestamp_source(pool.clone(), timestamp_source);
    let project_id = repositories
        .projects()
        .upsert_project("/tmp/fixed-clock", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    repositories
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let turn_metadata = crate::SessionTurnMetadata {
        applied_personality_id: None,
        applied_personality_prompt_hash: None,
        harness_turn_report: None,
        instruction_conversation_id: None,
        model: "gpt-5.6-sol".to_string(),
        provider_conversation_id: Some("conversation-a".to_string()),
        questions_json: "[]".to_string(),
        review_comment_resolutions: Vec::new(),
        token_usage_delta: SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            diff_state: SessionDiffState::Unknown,
            input_tokens: 3,
            output_tokens: 5,
        },
    };

    // Act
    repositories
        .sessions()
        .persist_session_turn_metadata("session-a", &turn_metadata)
        .await
        .expect("failed to persist turn metadata");
    repositories
        .sessions()
        .update_session_status_with_timing_at("session-a", "InProgress", 789)
        .await
        .expect("failed to update status");
    repositories
        .usage()
        .upsert_session_usage(
            "session-a",
            "review-model",
            &SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: SessionDiffState::Unknown,
                input_tokens: 7,
                output_tokens: 11,
            },
        )
        .await
        .expect("failed to persist usage");
    let session_timestamps = sqlx::query_as::<_, (i64, i64)>(
        "SELECT updated_at, in_progress_started_at FROM session WHERE id = ?",
    )
    .bind("session-a")
    .fetch_one(&pool)
    .await
    .expect("failed to load session timestamps");
    let usage_timestamps = sqlx::query_scalar::<_, i64>(
        "SELECT created_at FROM session_usage WHERE session_id = ? ORDER BY model",
    )
    .bind("session-a")
    .fetch_all(&pool)
    .await
    .expect("failed to load usage timestamps");

    // Assert
    assert_eq!(session_timestamps, (456, 789));
    assert_eq!(usage_timestamps, vec![456, 456]);
}

#[tokio::test]
async fn repository_parts_support_focused_adapter_injection() {
    // Arrange
    let pool = open_in_memory_pool(1)
        .await
        .expect("failed to open in-memory db");
    let baseline = AppRepositories::from_pool(pool);
    let mut operation = MockOperationRepository::<crate::DbError>::new();
    operation
        .expect_is_cancel_requested_for_operation()
        .withf(|operation_id| operation_id == "operation-a")
        .times(1)
        .returning(|_| Ok(true));
    let repositories = AppRepositories {
        run: Arc::clone(&baseline.run),
        activity: Arc::clone(&baseline.activity),
        operation: Arc::new(operation),
        project: Arc::clone(&baseline.project),
        review: Arc::clone(&baseline.review),
        session: Arc::clone(&baseline.session),
        setting: Arc::clone(&baseline.setting),
        usage: Arc::clone(&baseline.usage),
    };

    // Act
    let is_cancel_requested = repositories
        .operations()
        .is_cancel_requested_for_operation("operation-a")
        .await
        .expect("mock operation query should succeed");

    // Assert
    assert!(is_cancel_requested);
}

#[tokio::test]
async fn snapshot_fixtures_match_the_migrated_schema_and_stay_isolated() {
    // Arrange
    let migrated = open_in_memory_pool(1)
        .await
        .expect("failed to open migrated db");
    let database = Database::in_memory()
        .await
        .expect("failed to open snapshot db");
    let (_repositories, sibling) = AppRepositories::in_memory_with_pool()
        .await
        .expect("failed to open sibling snapshot db");

    // Act
    sqlx::query("CREATE TABLE isolation_probe (value INTEGER)")
        .execute(database.pool())
        .await
        .expect("failed to change snapshot schema");
    let foreign_keys = sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
        .fetch_one(database.pool())
        .await
        .expect("failed to read foreign-key pragma");
    let applied = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(&sibling)
        .await
        .expect("failed to count applied migrations");

    // Assert
    assert_eq!(schema(&sibling).await, schema(&migrated).await);
    assert!(
        schema(database.pool())
            .await
            .iter()
            .any(|object| object.starts_with("table:isolation_probe:"))
    );
    assert_eq!(foreign_keys, 1);
    assert_eq!(
        applied,
        i64::try_from(MIGRATOR.iter().count()).expect("bounded count")
    );
}

/// Lists schema objects with their definitions in a stable order.
async fn schema(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT type || ':' || name || COALESCE(':' || sql, '') FROM sqlite_master ORDER BY type, \
         name",
    )
    .fetch_all(pool)
    .await
    .expect("failed to read schema")
}
