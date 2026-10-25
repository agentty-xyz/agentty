use ag_session::GraphExecutionRef;

use crate::DbError;
use crate::execution_graph::support_test::produced;

#[tokio::test]
async fn progress_rejects_corrupt_execution_references_and_settled_responses() {
    for scenario in [
        "malformed-reference",
        "unlinked-reference",
        "malformed-response",
    ] {
        // Arrange
        let database = produced().await;
        if scenario == "malformed-response" {
            sqlx::query(
                "INSERT INTO execution_graph_response (execution_graph_execution_id, body) SELECT \
                 id, 'invalid response' FROM execution_graph_execution",
            )
            .execute(database.pool())
            .await
            .expect("corrupt settled response");
        } else {
            let reference = if scenario == "malformed-reference" {
                "invalid reference".to_string()
            } else {
                serde_json::json!(GraphExecutionRef::UtilityRun {
                    run_id: "missing".into()
                })
                .to_string()
            };
            sqlx::query("UPDATE execution_graph_execution SET reference = ?")
                .bind(reference)
                .execute(database.pool())
                .await
                .expect("corrupt execution reference");
        }

        // Act
        let result = database.graphs().progress("graph").await;

        // Assert
        assert!(
            matches!(
                result,
                Err(DbError::InvalidData {
                    entity: "execution graph",
                    ..
                })
            ),
            "{scenario}"
        );
    }
}
