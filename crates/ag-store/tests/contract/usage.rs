//! Public session usage accounting through the usage repository.

use ag_contracts::{ContextFill, CostBasis, CostEstimate, ModelUsage, TurnUsage, UsagePurpose};
use ag_store::Database;

/// One usage row's purpose and detail columns, as `session_usage` stores
/// them.
type DetailRow = (
    String,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    i64,
    i64,
    i64,
    i64,
    i64,
);

/// Verifies detail columns stay known only while every invocation reports
/// them, costs sum over priced invocations with a mixed basis, unreported
/// invocations are kept, and turns record the session context.
#[tokio::test]
async fn test_upsert_session_usage_accumulates_known_details_only() {
    // Arrange
    let database = Database::in_memory().await.expect("database");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("project");
    database
        .sessions()
        .insert_session("session-a", "model", "main", "Done", project_id)
        .await
        .expect("session");
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
        database
            .usage()
            .upsert_session_usage("session-a", purpose, &TurnUsage::single(model_usage))
            .await
            .expect("usage");
    }
    let rows: Vec<DetailRow> = sqlx::query_as(
        "SELECT purpose, cache_read_tokens, cache_write_tokens, reasoning_tokens, cost_micro_usd, \
         cost_basis, detailed_invocation_count, invocation_count, request_count, \
         unpriced_invocation_count, unreported_request_count FROM session_usage WHERE session_id \
         = 'session-a' ORDER BY model, purpose",
    )
    .fetch_all(database.pool())
    .await
    .expect("usage rows");
    let context: (Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT context_peak_tokens, context_window_tokens, (SELECT context_tokens FROM session \
         WHERE id = 'session-a') FROM session_usage WHERE purpose = 'turn'",
    )
    .fetch_one(database.pool())
    .await
    .expect("context");
    let loaded = database
        .usage()
        .load_session_usage("session-a")
        .await
        .expect("loaded usage");

    // Assert
    assert_eq!(
        rows,
        [
            (
                "review".to_string(),
                Some(450),
                Some(1),
                Some(4),
                Some(5),
                Some("list".to_string()),
                1,
                1,
                1,
                0,
                0
            ),
            (
                "turn".to_string(),
                None,
                None,
                None,
                Some(100),
                Some("mixed".to_string()),
                3,
                3,
                3,
                1,
                1
            ),
        ]
    );
    assert_eq!(context, (Some(400), Some(1_000), Some(200)));
    assert_eq!(
        loaded
            .iter()
            .map(|row| (row.input_tokens, row.output_tokens))
            .collect::<Vec<_>>(),
        [(900, 10), (600, 20)]
    );
}
