use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use ag_session::StartExecutionGraphRequest;

use crate::app::execution_graph::tests::{graph, seed_session};
use crate::app::graph_budget::GraphBudgetLedger;
use crate::infra::db::{DbError, MockGraphRepository};

#[tokio::test]
async fn failed_write_keeps_its_spending_until_a_retry_records_it() {
    for (failed_writes, recorded_calls) in [(1, 2), (usize::MAX, 0)] {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_test_app().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let recorded = Arc::new(AtomicI64::new(0));
        let mut repository = MockGraphRepository::new();
        repository.expect_record_budget_spending().returning({
            let (attempts, recorded) = (Arc::clone(&attempts), Arc::clone(&recorded));
            move |_, _, calls| {
                if attempts.fetch_add(1, Ordering::AcqRel) < failed_writes {
                    return Err(DbError::InvalidData {
                        entity: "execution graph",
                        reason: "database is busy".into(),
                    });
                }
                recorded.fetch_add(calls, Ordering::AcqRel);

                Ok(())
            }
        });
        app.services.set_graph_repository(Arc::new(repository));
        let budget = GraphBudgetLedger::budget(&app.services, "graph", "calls", 5);

        // Act
        budget.consume().expect("charge the initial call");
        budget.consume().expect("charge a retry");
        drop(budget);
        app.services.wait_for_cleanup_tasks(None).await;

        // Assert
        assert_eq!(recorded.load(Ordering::Acquire), recorded_calls);
        assert!(attempts.load(Ordering::Acquire) >= 2);
        // Spending the final flush could not record keeps relaunch blocked.
        assert_eq!(
            app.services
                .graph_budget_writers
                .has_unrecorded_spending("graph"),
            recorded_calls == 0
        );
    }
}

#[tokio::test]
async fn relaunch_is_rejected_while_a_stopped_pool_holds_unrecorded_spending() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let request = StartExecutionGraphRequest {
        graph: graph(),
        provider_call_limits: BTreeMap::from([("calls".to_string(), 5)]),
    };
    app.services
        .db()
        .graphs()
        .launch_graph(&request.graph, &request.provider_call_limits)
        .await
        .expect("launch");
    let stopped = GraphBudgetLedger::budget(&app.services, "graph", "calls", 5);
    let unspent = GraphBudgetLedger::budget(&app.services, "graph", "reviews", 1);
    stopped.consume().expect("charge a call");

    // Act
    let rejected = app.start_api_execution_graph(request).await;
    drop((stopped, unspent));
    app.services.wait_for_cleanup_tasks(None).await;

    // Assert
    assert!(
        rejected
            .expect_err("unrecorded spending")
            .to_string()
            .contains("not recorded yet")
    );
    assert!(
        !app.services
            .graph_budget_writers
            .has_unrecorded_spending("graph")
    );
    assert!(
        app.services
            .graph_drivers
            .lock()
            .expect("drivers")
            .is_empty()
    );
}

#[tokio::test]
async fn ledgered_budgets_persist_net_spending_after_their_last_clone_drops() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let limits = BTreeMap::from([("calls".to_string(), 5)]);
    app.services
        .db()
        .graphs()
        .launch_graph(&graph(), &limits)
        .await
        .expect("launch");
    let budget = GraphBudgetLedger::budget(&app.services, "graph", "calls", 5);
    let missing = GraphBudgetLedger::budget(&app.services, "missing", "calls", 1);

    // Act
    let unused = budget.reserve().expect("reserve");
    drop(unused);
    let reserved = budget.reserve().expect("reserve initial call");
    reserved.consume().expect("redeem initial call");
    budget.consume().expect("charge a retry");
    drop((budget, reserved));
    missing.consume().expect("charge an unknown graph");
    drop(missing);
    app.services.wait_for_cleanup_tasks(None).await;

    // Assert
    let remaining = app
        .services
        .db()
        .graphs()
        .launch_graph(&graph(), &limits)
        .await
        .expect("relaunch");
    assert_eq!(remaining["calls"], 3);
}
