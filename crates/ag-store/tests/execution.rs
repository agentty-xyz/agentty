//! Public worker/storage composition without provider binaries or a frontend.
use std::num::NonZeroUsize;
use std::sync::Arc;

use ag_runtime::{
    AgentRequestKind, OneShotClient, OneShotError, OneShotRequest, OneShotSubmission,
    PermissionMode, ReasoningLevel, SpeedMode,
};
use ag_store::Database;
use ag_worker::{HeartbeatClock, RunScope, RunWorker, scoped_client};
use async_trait::async_trait;

struct Runtime;

#[async_trait]
impl OneShotClient for Runtime {
    async fn submit(&self, request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        assert_eq!(request.harness, "custom-harness");
        assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
        Err(OneShotError::new("provider unavailable"))
    }
}

#[tokio::test]
async fn public_worker_composition_records_project_utility_failure() {
    // Arrange
    let db = Database::open_in_memory().await.expect("database");
    let worker = Arc::new(RunWorker::new(
        Arc::new(Runtime),
        db.runs(),
        Arc::new(HeartbeatClock),
        NonZeroUsize::MIN,
    ));
    let client = scoped_client(
        worker.clone(),
        RunScope {
            parent_id: Some("project-operation".into()),
            purpose: Some("conflict assistance".into()),
            ..RunScope::default()
        },
    );
    // Act
    let result = client
        .submit(OneShotRequest {
            child_pid: None,
            folder: "repository".into(),
            harness: "custom-harness".into(),
            model: "model".into(),
            permission_mode: PermissionMode::ReadOnly,
            prompt: "inspect".into(),
            provider_call_budget: None,
            reasoning_level: ReasoningLevel::default(),
            request_kind: AgentRequestKind::UtilityPrompt,
            speed_mode: SpeedMode::default(),
        })
        .await;
    worker.shutdown().await;
    db.runs().recover().await.expect("recovery");
    // Assert
    assert_eq!(
        result.expect_err("provider error").to_string(),
        "provider unavailable"
    );
    let row: (String, String, String, String) =
        sqlx::query_as("SELECT status, parent_id, purpose, last_error FROM agent_run")
            .fetch_one(db.pool())
            .await
            .expect("durable run");
    assert_eq!(
        row,
        (
            "failed".into(),
            "project-operation".into(),
            "conflict assistance".into(),
            "provider unavailable".into()
        )
    );
}
