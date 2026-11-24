//! Session usage tests: per-model and per-purpose accumulation, detail
//! columns, session context, and cleanup on session delete.

use ag_contracts::{ContextFill, CostBasis, CostEstimate, ModelUsage, TurnUsage, UsagePurpose};

use super::support::insert_session_fixture;
use crate::connection::Database;
use crate::test_support::token_usage;

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
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].session_id.as_deref(), Some("session-a"));
}

/// Verifies detail columns stay known only while every invocation reports
/// them, costs sum over priced invocations with a mixed basis, unreported
/// invocations are kept, and turns record the session context.
#[tokio::test]
async fn test_upsert_session_usage_accumulates_known_details_only() {
    // Arrange
    let database = session_database().await;
    let usage = database.usage();
    let detailed = |micro_usd, basis, input_tokens| ModelUsage {
        cache_read_tokens: Some(input_tokens / 2),
        cache_write_tokens: Some(1),
        context: Some(ContextFill::of_request(input_tokens, Some(1_000))),
        cost: Some(CostEstimate { basis, micro_usd }),
        input_tokens,
        output_tokens: 10,
        reasoning_tokens: Some(4),
        requests: 1,
        ..ModelUsage::new("model")
    };

    // Act
    for (purpose, model_usage) in [
        (UsagePurpose::Turn, detailed(70, CostBasis::ListPrice, 400)),
        (
            UsagePurpose::Turn,
            detailed(30, CostBasis::AgentReported, 200),
        ),
        (UsagePurpose::Turn, ModelUsage::unreported("model", 1)),
        (UsagePurpose::Review, detailed(5, CostBasis::ListPrice, 900)),
    ] {
        usage
            .upsert_session_usage("session-a", purpose, &TurnUsage::single(model_usage))
            .await
            .expect("failed to record usage");
    }
    let rows = detail_rows(&database).await;
    let totals = session_totals(&database).await;

    // Assert
    assert_eq!(
        rows[1],
        DetailRow {
            cache_read_tokens: None,
            cache_write_tokens: None,
            context_peak_tokens: Some(400),
            context_window_tokens: Some(1_000),
            cost_basis: Some("mixed".to_string()),
            cost_micro_usd: Some(100),
            detailed_invocation_count: 3,
            input_tokens: 600,
            invocation_count: 3,
            model: "model".to_string(),
            output_tokens: 20,
            purpose: "turn".to_string(),
            reasoning_tokens: None,
            request_count: 3,
            unpriced_invocation_count: 1,
            unreported_request_count: 1,
        }
    );
    assert_eq!(rows[0].purpose, "review");
    assert_eq!(rows[0].cache_read_tokens, Some(450));
    assert_eq!(rows[0].cost_basis.as_deref(), Some("list"));
    assert_eq!(totals, (1_500, 30, Some(200), Some(1_000)));
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
