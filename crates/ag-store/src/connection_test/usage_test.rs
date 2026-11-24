//! Session usage tests: per-model and per-purpose accumulation and cleanup on
//! session delete.

use ag_contracts::test_support::token_usage;
use ag_contracts::{ModelUsage, TurnUsage, UsagePurpose};

use super::support::insert_session_fixture;
use crate::connection::Database;

/// Verifies `delete_session()` removes the session row and nulls
/// `session_usage.session_id`.
#[tokio::test]
async fn test_delete_session_removes_row_and_nulls_usage_foreign_key() {
    // Arrange
    let database = session_database().await;
    database
        .usage()
        .upsert_session_usage(
            "session-a",
            UsagePurpose::Turn,
            &token_usage("claude-opus-4.1", 11, 29),
        )
        .await
        .expect("failed to insert usage row");

    // Act
    database
        .sessions()
        .delete_session("session-a")
        .await
        .expect("failed to delete session");
    let deleted_session = database
        .sessions()
        .load_session_timestamps("session-a")
        .await
        .expect("failed to load deleted session timestamps");
    let retained_usage_row = sqlx::query_as!(
        SessionUsageSessionIdRow,
        r#"
SELECT session_id AS "session_id: _"
FROM session_usage
WHERE model = ?
"#,
        "claude-opus-4.1"
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load retained usage row");

    // Assert
    assert_eq!(deleted_session, None);
    assert_eq!(retained_usage_row.session_id, None,);
}

/// Verifies `upsert_session_usage()` accumulates per-model token totals and
/// invocation counts, skips empty usage, and keeps purposes apart.
#[tokio::test]
async fn test_upsert_session_usage_accumulates_counts_per_model_and_purpose() {
    // Arrange
    let database = session_database().await;
    let usage = database.usage();
    usage
        .upsert_session_usage(
            "session-a",
            UsagePurpose::Turn,
            &token_usage("claude-opus-4.1", 11, 29),
        )
        .await
        .expect("failed to insert first usage row");
    usage
        .upsert_session_usage(
            "session-a",
            UsagePurpose::Turn,
            &token_usage("claude-opus-4.1", 3, 5),
        )
        .await
        .expect("failed to update existing usage row");
    usage
        .upsert_session_usage(
            "session-a",
            UsagePurpose::Title,
            &token_usage("claude-opus-4.1", 7, 1),
        )
        .await
        .expect("failed to insert title usage row");
    usage
        .upsert_session_usage(
            "session-a",
            UsagePurpose::Turn,
            &TurnUsage::single(ModelUsage::new("ignored-model")),
        )
        .await
        .expect("failed to ignore zero-usage update");

    // Act
    let rows = detail_rows(&database).await;
    let totals = session_totals(&database).await;

    // Assert
    assert_eq!(
        rows,
        [
            DetailRow {
                detailed_invocation_count: 1,
                invocation_count: 1,
                input_tokens: 7,
                model: "claude-opus-4.1".to_string(),
                output_tokens: 1,
                purpose: "title".to_string(),
                request_count: 1,
                unpriced_invocation_count: 1,
                ..DetailRow::default()
            },
            DetailRow {
                detailed_invocation_count: 2,
                invocation_count: 2,
                input_tokens: 14,
                model: "claude-opus-4.1".to_string(),
                output_tokens: 34,
                purpose: "turn".to_string(),
                request_count: 2,
                unpriced_invocation_count: 2,
                ..DetailRow::default()
            },
        ]
    );
    assert_eq!(totals, (21, 35, None, None));
    let loaded = usage
        .load_session_usage("session-a")
        .await
        .expect("failed to load session usage");
    assert_eq!(
        loaded
            .iter()
            .map(|row| (row.session_id.as_deref(), row.purpose.as_str()))
            .collect::<Vec<_>>(),
        [(Some("session-a"), "title"), (Some("session-a"), "turn")]
    );
}

/// Opens a migrated database holding the `session-a` session.
async fn session_database() -> Database {
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "session-a", "main", "Done", project_id).await;

    database
}

/// Loads every usage row of `session-a`, ordered by model and purpose.
async fn detail_rows(database: &Database) -> Vec<DetailRow> {
    sqlx::query_as!(
        DetailRow,
        r#"
SELECT cache_read_tokens, cache_write_tokens, context_peak_tokens, context_window_tokens,
       cost_basis, cost_micro_usd, detailed_invocation_count, input_tokens, invocation_count,
       model, output_tokens, purpose, reasoning_tokens, request_count,
       unpriced_invocation_count, unreported_request_count
FROM session_usage
WHERE session_id = 'session-a'
ORDER BY model, purpose
"#
    )
    .fetch_all(database.pool())
    .await
    .expect("failed to load usage rows")
}

/// Loads the token totals and context columns of `session-a`.
async fn session_totals(database: &Database) -> (i64, i64, Option<i64>, Option<i64>) {
    let row = sqlx::query!(
        "SELECT input_tokens, output_tokens, context_tokens, context_window_tokens FROM session \
         WHERE id = 'session-a'"
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load session totals");

    (
        row.input_tokens,
        row.output_tokens,
        row.context_tokens,
        row.context_window_tokens,
    )
}

/// Typed helper row used to verify nullable session references.
struct SessionUsageSessionIdRow {
    session_id: Option<String>,
}

/// Every accumulated column of one usage row.
#[derive(Debug, Default, Eq, PartialEq)]
struct DetailRow {
    cache_read_tokens: Option<i64>,
    cache_write_tokens: Option<i64>,
    context_peak_tokens: Option<i64>,
    context_window_tokens: Option<i64>,
    cost_basis: Option<String>,
    cost_micro_usd: Option<i64>,
    detailed_invocation_count: i64,
    input_tokens: i64,
    invocation_count: i64,
    model: String,
    output_tokens: i64,
    purpose: String,
    reasoning_tokens: Option<i64>,
    request_count: i64,
    unpriced_invocation_count: i64,
    unreported_request_count: i64,
}
