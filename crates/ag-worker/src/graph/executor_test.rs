use std::collections::HashMap;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::Path;
use std::sync::Arc;

use ag_contracts::{OneShotError, PermissionMode, ProviderCallBudget};

use crate::graph::model::{MockGraphDispatchRepository, MockGraphNodeRunner};
use crate::graph::support_test::{dispatch, graph};
use crate::graph::{GraphAdmission, GraphExecutor, GraphResources};

#[tokio::test]
async fn unclaimed_admissions_refund_the_initial_provider_call() {
    for admission in [
        Ok(GraphAdmission::Blocked),
        Ok(GraphAdmission::Existing(dispatch().record)),
        Err(OneShotError::new("claim failed")),
    ] {
        // Arrange
        let fails = admission.is_err();
        let mut graph = graph();
        graph.nodes[0].execution.shared_budget_id = Some("budget".into());
        let budget = ProviderCallBudget::new(1);
        let resources = Arc::new(GraphResources::new(
            NonZeroUsize::MIN,
            HashMap::from([("budget".into(), budget.clone())]),
        ));
        let mut repository = MockGraphDispatchRepository::new();
        repository.expect_graph().once().return_const(Ok(graph));
        let claimed_budget = budget.clone();
        repository
            .expect_claim()
            .once()
            .returning(move |_, _, _, _, _| {
                assert_eq!(claimed_budget.remaining(), 0, "reserved before claim");
                admission.clone()
            });
        let mut runner = MockGraphNodeRunner::new();
        runner.expect_supports().once().return_const(true);
        let executor = GraphExecutor::new(Arc::new(repository), resources, Arc::new(runner));

        // Act
        let result = executor
            .run_ready(
                "graph",
                NonZeroU64::MIN,
                Path::new("workspace"),
                PermissionMode::ReadOnly,
            )
            .await;

        // Assert
        assert_eq!(result.is_err(), fails);
        if let Ok(receipts) = result {
            assert_eq!(receipts.len(), 0);
        }
        assert_eq!(budget.remaining(), 1, "unused reservation is refunded");
    }
}

#[tokio::test]
async fn exhausted_budget_defers_admission_and_releases_resource_leases() {
    // Arrange
    let mut graph = graph();
    graph.nodes[0].execution.shared_budget_id = Some("budget".into());
    let resources = Arc::new(GraphResources::new(
        NonZeroUsize::MIN,
        HashMap::from([("budget".into(), ProviderCallBudget::new(0))]),
    ));
    let environment = resources.environment(&graph.nodes[0].env_id);
    let mut repository = MockGraphDispatchRepository::new();
    repository.expect_graph().once().return_const(Ok(graph));
    let mut runner = MockGraphNodeRunner::new();
    runner.expect_supports().once().return_const(true);
    let executor = GraphExecutor::new(Arc::new(repository), resources.clone(), Arc::new(runner));

    // Act
    let receipts = executor
        .run_ready(
            "graph",
            NonZeroU64::MIN,
            Path::new("workspace"),
            PermissionMode::ReadOnly,
        )
        .await
        .expect("exhausted budget defers work");

    // Assert
    assert_eq!(receipts.len(), 0);
    assert_eq!(resources.capacity.available_permits(), 1);
    assert!(environment.try_write_owned().is_ok());
}

#[tokio::test]
async fn env_writer_blocks_read_admission_without_consuming_capacity() {
    // Arrange
    let graph = graph();
    let resources = Arc::new(GraphResources::new(NonZeroUsize::MIN, HashMap::new()));
    let environment = resources.environment(&graph.nodes[0].env_id);
    let writer = environment.clone().try_write_owned().expect("writer lease");
    let mut repository = MockGraphDispatchRepository::new();
    repository.expect_graph().once().return_const(Ok(graph));
    let mut runner = MockGraphNodeRunner::new();
    runner.expect_supports().once().return_const(true);
    let executor = GraphExecutor::new(Arc::new(repository), resources.clone(), Arc::new(runner));

    // Act
    let receipts = executor
        .run_ready(
            "graph",
            NonZeroU64::MIN,
            Path::new("workspace"),
            PermissionMode::ReadOnly,
        )
        .await
        .expect("blocked wave");

    // Assert
    assert_eq!(receipts.len(), 0);
    assert_eq!(resources.capacity.available_permits(), 1);
    drop(writer);
    assert!(environment.try_read_owned().is_ok());
}

#[tokio::test]
async fn panicked_node_settles_failure_before_releasing_resource_leases() {
    for settlement_fails in [false, true] {
        // Arrange
        let graph = graph();
        let resources = Arc::new(GraphResources::new(NonZeroUsize::MIN, HashMap::new()));
        let environment = resources.environment(&graph.nodes[0].env_id);
        let mut repository = MockGraphDispatchRepository::new();
        repository.expect_graph().once().return_const(Ok(graph));
        repository
            .expect_claim()
            .once()
            .return_const(Ok(GraphAdmission::Claimed(Box::new(dispatch()))));
        let settlement_resources = resources.clone();
        let settlement_environment = environment.clone();
        repository
            .expect_fail()
            .once()
            .returning(move |record, error| {
                assert_eq!(record, &dispatch().record);
                assert_eq!(error, "Graph node runner panicked");
                assert_eq!(settlement_resources.capacity.available_permits(), 0);
                assert!(settlement_environment.try_write().is_err());

                if settlement_fails {
                    Err(OneShotError::new("settlement failed"))
                } else {
                    Ok(())
                }
            });
        let mut runner = MockGraphNodeRunner::new();
        runner.expect_supports().once().return_const(true);
        runner.expect_run().once().returning(|_| {
            std::panic::resume_unwind(Box::new("graph worker panicked"));
        });
        let executor =
            GraphExecutor::new(Arc::new(repository), resources.clone(), Arc::new(runner));

        // Act
        let result = executor
            .run_ready(
                "graph",
                NonZeroU64::MIN,
                Path::new("workspace"),
                PermissionMode::ReadOnly,
            )
            .await;

        // Assert
        assert_eq!(
            result.expect_err("runner failure").to_string(),
            if settlement_fails {
                "settlement failed"
            } else {
                "Graph node runner panicked"
            }
        );
        assert_eq!(resources.capacity.available_permits(), 1);
        assert!(environment.try_write_owned().is_ok());
    }
}

#[tokio::test]
async fn admission_panic_returns_a_worker_error_and_releases_resource_leases() {
    // Arrange
    let graph = graph();
    let resources = Arc::new(GraphResources::new(NonZeroUsize::MIN, HashMap::new()));
    let environment = resources.environment(&graph.nodes[0].env_id);
    let mut repository = MockGraphDispatchRepository::new();
    repository.expect_graph().once().return_const(Ok(graph));
    repository
        .expect_claim()
        .once()
        .returning(|_, _, _, _, _| std::panic::resume_unwind(Box::new("admission panicked")));
    let mut runner = MockGraphNodeRunner::new();
    runner.expect_supports().once().return_const(true);
    let executor = GraphExecutor::new(Arc::new(repository), resources.clone(), Arc::new(runner));

    // Act
    let result = executor
        .run_ready(
            "graph",
            NonZeroU64::MIN,
            Path::new("workspace"),
            PermissionMode::ReadOnly,
        )
        .await;

    // Assert
    assert!(
        result
            .expect_err("worker panic")
            .to_string()
            .contains("panicked")
    );
    assert_eq!(resources.capacity.available_permits(), 1);
    assert!(environment.try_write_owned().is_ok());
}
