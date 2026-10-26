use std::collections::HashMap;
use std::future::Future;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::Path;
use std::sync::Arc;
use std::task::{Context, Waker};

use ag_contracts::{OneShotError, PermissionMode, ProviderCallBudget};
use tokio::sync::Notify;

use crate::graph::model::{MockGraphDispatchRepository, MockGraphNodeRunner};
use crate::graph::support_test::{dispatch, graph};
use crate::graph::{GraphAdmission, GraphExecutor, GraphResources};

#[test]
fn budget_progress_observes_shared_credit_without_reserving_it() {
    // Arrange
    let budget = ProviderCallBudget::new(1);
    let executor = GraphExecutor::new(
        Arc::new(MockGraphDispatchRepository::new()),
        Arc::new(GraphResources::new(
            NonZeroUsize::MIN,
            HashMap::from([("calls".into(), budget.clone())]),
        )),
        Arc::new(MockGraphNodeRunner::new()),
    );

    // Act / Assert
    assert_eq!(executor.remaining_provider_calls("missing"), None);
    assert_eq!(executor.remaining_provider_calls("calls"), Some(1));
    budget.consume().expect("consume");
    assert_eq!(executor.remaining_provider_calls("calls"), Some(0));
}

#[tokio::test]
async fn capacity_releases_notify_only_registered_waiters_once() {
    // Arrange
    let resources = GraphResources::new(NonZeroUsize::MIN, HashMap::new());
    let owner = Arc::new(Notify::new());
    let waiting = Arc::new(Notify::new());
    let canceled = Arc::new(Notify::new());
    let permit = resources.try_capacity(&owner).expect("initial permit");
    assert!(resources.try_capacity(&canceled).is_none());
    drop(canceled);
    assert!(resources.try_capacity(&waiting).is_none());
    assert!(resources.try_capacity(&waiting).is_none());

    // Act: return before the contending coordinator starts waiting.
    drop(permit);
    waiting.notified().await;
    let permit = resources.try_capacity(&waiting).expect("capacity returned");
    drop(permit);

    // Assert: idle and duplicate scans cannot keep waking one another.
    let another_wake = tokio::select! {
        biased;
        () = waiting.notified() => true,
        () = owner.notified() => true,
        () = std::future::ready(()) => false,
    };
    assert!(!another_wake);
    assert_eq!(resources.capacity.available_permits(), 1);
}

#[tokio::test]
async fn environment_releases_notify_only_registered_contenders_once() {
    // Arrange
    let resources = Arc::new(GraphResources::new(NonZeroUsize::MIN, HashMap::new()));
    let executor = || {
        GraphExecutor::new(
            Arc::new(MockGraphDispatchRepository::new()),
            resources.clone(),
            Arc::new(MockGraphNodeRunner::new()),
        )
    };
    let owner = executor();
    let waiting = executor();
    let canceled = executor();
    let mut writer = graph().nodes.remove(0);
    writer.execution.workspace_access = ag_session::GraphWorkspaceAccess::ExclusiveWrite;
    let lease = owner.try_environment(&writer).expect("owner lease");
    assert!(canceled.try_environment(&writer).is_err());
    assert!(waiting.try_environment(&writer).is_err());
    assert!(waiting.try_environment(&writer).is_err());
    drop(canceled);

    // Act: release before the competing driver starts waiting.
    drop(lease);
    waiting.environment_released().await;
    let lease = waiting.try_environment(&writer).expect("returned Env");
    drop(lease);

    // Assert: idle scans and duplicate registrations do not broadcast.
    let another_wake = tokio::select! {
        biased;
        () = waiting.environment_released() => true,
        () = owner.environment_released() => true,
        () = std::future::ready(()) => false,
    };
    assert!(!another_wake);
}

#[tokio::test]
async fn environment_waiters_ignore_releases_until_the_conflicting_gate_is_available() {
    for background_blocks in [false, true] {
        // Arrange
        let resources = Arc::new(GraphResources::new(NonZeroUsize::MIN, HashMap::new()));
        let executor = || {
            GraphExecutor::new(
                Arc::new(MockGraphDispatchRepository::new()),
                resources.clone(),
                Arc::new(MockGraphNodeRunner::new()),
            )
        };
        let owner = executor();
        let waiting = executor();
        let node = graph().nodes.remove(0);
        let first = owner.try_environment(&node).expect("first reader");
        let second = owner.try_environment(&node).expect("second reader");
        let background = if background_blocks {
            Some(
                resources
                    .background_readers(&node.env_id)
                    .read_owned()
                    .await,
            )
        } else {
            None
        };
        let mut writer = node.clone();
        writer.execution.workspace_access = ag_session::GraphWorkspaceAccess::ExclusiveWrite;
        assert!(waiting.try_environment(&writer).is_err());

        // Act / Assert: partial or unrelated releases cannot cause idle loops.
        drop(first);
        let premature = tokio::select! {
            biased;
            () = waiting.environment_released() => true,
            () = std::future::ready(()) => false,
        };
        assert!(!premature);
        drop(second);
        let notified = tokio::select! {
            biased;
            () = waiting.environment_released() => true,
            () = std::future::ready(()) => false,
        };
        assert_eq!(notified, !background_blocks);
        drop(background);
        assert!(waiting.try_environment(&writer).is_ok());
    }
}

#[tokio::test]
async fn idle_scans_finish_before_waking_capacity_waiters() {
    // Arrange
    let mut graph = graph();
    let mut second = graph.nodes[0].clone();
    second.id = "second".into();
    graph.nodes.push(second);
    let resources = Arc::new(GraphResources::new(NonZeroUsize::MIN, HashMap::new()));
    let waiting = Arc::new(Notify::new());
    let mut repository = MockGraphDispatchRepository::new();
    repository.expect_graph().once().return_const(Ok(graph));
    let checked_resources = resources.clone();
    let checked_waiting = waiting.clone();
    let mut claims = 0;
    repository
        .expect_claim()
        .times(2)
        .returning(move |_, _, _, _, _| {
            claims += 1;
            if claims == 1 {
                assert!(checked_resources.try_capacity(&checked_waiting).is_none());
            } else {
                let mut notified = std::pin::pin!(checked_waiting.notified());
                assert!(
                    notified
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop()))
                        .is_pending(),
                    "an idle prefix must not repeatedly wake competing scans"
                );
            }

            Ok(GraphAdmission::Blocked)
        });
    let mut runner = MockGraphNodeRunner::new();
    runner.expect_supports().times(2).return_const(true);
    let executor = GraphExecutor::new(Arc::new(repository), resources, Arc::new(runner));

    // Act
    let receipts = executor
        .run_ready(
            "graph",
            NonZeroU64::MIN,
            Path::new("workspace"),
            PermissionMode::ReadOnly,
        )
        .await
        .expect("idle scan");

    // Assert
    assert_eq!(receipts.len(), 0);
    waiting.notified().await;
}

#[tokio::test]
async fn session_adapters_acquire_their_environment_after_serial_selection() {
    // Arrange
    let mut graph = graph();
    graph.nodes[0].execution.context = ag_session::GraphContext::TaskConversation {
        task_id: "task".into(),
    };
    graph.nodes[0].execution.response_profile = ag_protocol::ProtocolRequestProfile::SessionTurn;
    let resources = Arc::new(GraphResources::new(NonZeroUsize::MIN, HashMap::new()));
    let child_resources = Arc::new(resources.with_budgets(HashMap::new()));
    let env = resources.environment(&graph.nodes[0].env_id);
    assert!(Arc::ptr_eq(
        &env,
        &child_resources.environment(&graph.nodes[0].env_id)
    ));
    let _earlier_writer = env.write_owned().await;
    let mut repository = MockGraphDispatchRepository::new();
    repository.expect_graph().return_const(Ok(graph.clone()));
    let mut dispatch = dispatch();
    dispatch.node = graph.nodes[0].clone();
    let mut claims = 0;
    repository.expect_claim().returning(move |_, _, _, _, _| {
        claims += 1;
        if claims == 1 {
            Ok(GraphAdmission::Claimed(Box::new(dispatch.clone())))
        } else {
            Ok(GraphAdmission::Blocked)
        }
    });
    repository.expect_finish().once().return_const(Ok(()));
    let mut runner = MockGraphNodeRunner::new();
    runner.expect_supports().return_const(true);
    runner.expect_manages_environment().return_const(true);
    runner
        .expect_run()
        .once()
        .returning(|_| Ok("response".into()));
    let executor = GraphExecutor::new(Arc::new(repository), child_resources, Arc::new(runner));

    // Act
    let receipts = executor
        .run_ready(
            "graph",
            graph.revision,
            Path::new("workspace"),
            PermissionMode::ReadOnly,
        )
        .await
        .expect("serial handoff");

    // Assert
    assert_eq!(
        receipts.len(),
        1,
        "the queued command must not hold the lease needed by earlier work"
    );
}

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
        let releases = resources.subscribe_releases();
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
        assert!(
            !releases
                .has_changed()
                .expect("unclaimed scans do not wake drivers")
        );
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
async fn graph_writers_exclude_background_readers_and_release_partial_leases() {
    for access in [
        ag_session::GraphWorkspaceAccess::SharedRead,
        ag_session::GraphWorkspaceAccess::ExclusiveWrite,
    ] {
        // Arrange
        let mut node = graph().nodes.remove(0);
        node.execution.workspace_access = access;
        let resources = Arc::new(GraphResources::new(NonZeroUsize::MIN, HashMap::new()));
        let child_resources = Arc::new(resources.with_budgets(HashMap::new()));
        let background = resources.background_readers(&node.env_id);
        assert!(Arc::ptr_eq(
            &background,
            &child_resources.background_readers(&node.env_id)
        ));
        let environment = resources.environment(&node.env_id);
        let executor = GraphExecutor::new(
            Arc::new(MockGraphDispatchRepository::new()),
            child_resources,
            Arc::new(MockGraphNodeRunner::new()),
        );
        let background_reader = background.clone().read_owned().await;

        // Act / Assert: legacy readers overlap host writers and graph readers.
        assert!(environment.clone().try_write_owned().is_ok());
        let lease = executor.try_environment(&node);
        assert_eq!(
            lease.is_ok(),
            access == ag_session::GraphWorkspaceAccess::SharedRead
        );
        drop(lease);
        drop(background_reader);

        // A failed second acquire must release the first graph writer gate.
        let host_writer = environment.clone().write_owned().await;
        assert!(executor.try_environment(&node).is_err());
        assert!(background.clone().try_read_owned().is_ok());
        drop(host_writer);
        let lease = executor.try_environment(&node).expect("graph lease");
        assert!(environment.try_write_owned().is_err());
        assert_eq!(
            background.clone().try_read_owned().is_ok(),
            access == ag_session::GraphWorkspaceAccess::SharedRead
        );
        drop(lease);
        assert!(background.try_write_owned().is_ok());
    }
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
        let releases = resources.subscribe_releases();
        let settlement_releases = releases.clone();
        repository
            .expect_fail()
            .once()
            .returning(move |record, error| {
                assert_eq!(record, &dispatch().record);
                assert_eq!(error, "Graph node runner panicked");
                assert_eq!(settlement_resources.capacity.available_permits(), 0);
                assert!(settlement_environment.try_write().is_err());
                assert!(
                    !settlement_releases
                        .has_changed()
                        .expect("no wake before settlement")
                );

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
        assert!(
            releases
                .has_changed()
                .expect("failed node wakes drivers after releasing leases")
        );
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
