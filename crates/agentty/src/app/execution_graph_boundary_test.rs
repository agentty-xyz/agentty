use std::collections::BTreeMap;
use std::future;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ag_contracts::{OneShotError, PermissionMode};
use ag_protocol::ProtocolRequestProfile;
use ag_session::{
    GraphContext, GraphExecutionRef, GraphNode, GraphResultRef, StartExecutionGraphRequest,
};
use ag_worker::test_support::{AppServerTurnResponse, MockAppServerClient};
use ag_worker::{GraphExecutor, GraphNodeRunner, GraphRunRequest};
use async_trait::async_trait;
use sqlx::SqlitePool;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::{graph, run_request, seed_session, wait_result};
use crate::app::App;
use crate::infra::db::Database;
use crate::test_support::{new_test_app, new_test_app_with_clients, test_app_clients};

#[tokio::test]
async fn graph_launch_propagates_storage_and_permission_failures_without_dispatch() {
    for scenario in [
        "missing-row",
        "closed-pool",
        "invalid-permissions",
        "conflicting-snapshot",
    ] {
        // Arrange
        let (mut app, pool, _directory) = app_with_pool().await;
        let mut snapshot = graph();
        match scenario {
            "missing-row" => app
                .services
                .db()
                .sessions()
                .delete_session("sess")
                .await
                .expect("remove row"),
            "closed-pool" => pool.close().await,
            "invalid-permissions" => {
                sqlx::query("UPDATE session SET permission_mode = 'invalid' WHERE id = 'sess'")
                    .execute(&pool)
                    .await
                    .expect("corrupt stored permission");
            }
            _ => {
                app.services
                    .db()
                    .graphs()
                    .save_graph(&snapshot, None)
                    .await
                    .expect("saved snapshot");
                snapshot.nodes[0].objective.push_str("conflicting");
            }
        }

        // Act
        let result = app
            .start_api_execution_graph(StartExecutionGraphRequest {
                graph: snapshot,
                provider_call_limits: BTreeMap::from([("calls".into(), 1)]),
            })
            .await;

        // Assert
        assert!(result.is_err(), "{scenario}");
        assert_eq!(app.services.graph_drivers.lock().expect("drivers").len(), 0);
        app.services.wait_for_cleanup_tasks(None).await;
    }
}

#[tokio::test]
async fn acceptance_and_handoff_rejections_reply_without_advancing_work() {
    for closed in [false, true] {
        // Arrange
        let (mut app, pool, _directory) = app_with_pool().await;
        app.services
            .db()
            .graphs()
            .save_graph(&graph(), None)
            .await
            .expect("snapshot");
        let reference = GraphResultRef {
            execution: run_request().dispatch.record.execution,
            graph_id: "graph".into(),
            graph_revision: NonZeroU64::MIN,
            id: "unvalidated".into(),
            node_id: "first".into(),
            output_name: "response".into(),
            schema: ProtocolRequestProfile::SessionTurn,
            version: NonZeroU64::MIN,
        };
        if closed {
            pool.close().await;
        }
        let wake = app.services.graph_wake.subscribe();

        // Act / Assert: only durable acceptance may wake dependents.
        assert!(
            app.accept_api_execution_graph_result(reference)
                .await
                .is_err()
        );
        assert!(!wake.has_changed().expect("acceptance wake"));
        let mut request = run_request();
        if !closed {
            request.dispatch.record.execution = GraphExecutionRef::UtilityRun {
                run_id: "utility".into(),
            };
        }
        let (admitted_tx, admitted_rx) = oneshot::channel();
        let (reply_tx, reply_rx) = oneshot::channel();
        app.enqueue_graph_node(request, CancellationToken::new(), admitted_tx, reply_tx)
            .await;
        admitted_rx.await.expect("handoff rejection acknowledged");
        let error = reply_rx
            .await
            .expect("failure reply")
            .expect_err("rejected handoff")
            .to_string();
        assert!(error.contains(if closed {
            "closed"
        } else {
            "requires a task attempt"
        }));
        app.services.wait_for_cleanup_tasks(None).await;
    }
}

#[tokio::test]
async fn fresh_graph_utilities_use_the_worker_and_keep_task_history_empty() {
    // Arrange
    let mut provider = MockAppServerClient::new();
    provider
        .expect_run_isolated_turn()
        .once()
        .returning(|request, _| {
            request
                .provider_call_budget
                .expect("captured budget")
                .consume()
                .expect("provider credit");
            Box::pin(async {
                Ok(AppServerTurnResponse {
                    assistant_message: r#"{"answer":"fresh response"}"#.into(),
                    context_reset: false,
                    input_tokens: 1,
                    output_tokens: 1,
                    pid: None,
                    provider_conversation_id: None,
                })
            })
        });
    provider
        .expect_shutdown_session()
        .returning(|_| Box::pin(async {}));
    let clients = test_app_clients().with_app_server_client_override(Arc::new(provider));
    let (mut app, _directory) = new_test_app_with_clients(clients).await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let mut snapshot = graph();
    snapshot.nodes.truncate(1);
    snapshot.nodes[0].execution.context = GraphContext::Fresh;
    snapshot.nodes[0].execution.response_profile = ProtocolRequestProfile::UtilityPrompt;
    snapshot.nodes[0].outputs[0].schema = ProtocolRequestProfile::UtilityPrompt;
    snapshot.nodes[0].request.attachments.clear();
    let service = app.session_service();

    // Act
    app.drive_session_request(service.start_execution_graph(StartExecutionGraphRequest {
        graph: snapshot,
        provider_call_limits: BTreeMap::from([("calls".into(), 1)]),
    }))
    .await
    .expect("fresh launch");
    let result = app
        .drive_session_request(wait_result(
            app.services.db().clone(),
            app.services.graph_wake.subscribe(),
            "first",
        ))
        .await;

    // Assert
    assert!(matches!(
        result.result.reference.execution,
        GraphExecutionRef::UtilityRun { .. }
    ));
    assert!(result.body.contains("fresh response"));
    assert_eq!(
        app.services
            .db()
            .tasks()
            .list_tasks("sess")
            .await
            .expect("tasks")
            .len(),
        0
    );
    app.services.stop_graph_drivers(Some("other-session"));
    assert_eq!(
        app.services
            .graph_drivers
            .lock()
            .expect("retained driver")
            .len(),
        1
    );
    app.services.wait_for_cleanup_tasks(None).await;
}

#[tokio::test]
async fn paused_graph_drivers_resume_or_cancel_without_dispatching_early() {
    for scenario in ["resume", "cancel-paused", "already-canceled"] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        seed_session(&mut app.sessions, app.services.db()).await;
        app.services
            .db()
            .sessions()
            .update_session_status_with_timing_at("sess", "Question", 0)
            .await
            .expect("paused session");
        assert_eq!(app.services.graph_session_permission("sess").await, None);
        let snapshot = graph();
        app.services
            .db()
            .graphs()
            .save_graph(&snapshot, None)
            .await
            .expect("snapshot");
        let checks = Arc::new(AtomicUsize::new(0));
        let runner = Arc::new(UnsupportedRunner {
            checks: checks.clone(),
        });
        let executor = GraphExecutor::new(
            app.services.db().graph_dispatch(),
            app.services.graph_resources.clone(),
            runner,
        );
        let cancellation = CancellationToken::new();
        if scenario == "already-canceled" {
            cancellation.cancel();
        }
        let mut reads = 0;
        let driver = app.services.drive_graph(
            &snapshot,
            &cancellation,
            &executor,
            Path::new("workspace"),
            || {
                reads += 1;
                future::ready((reads > 1).then_some(PermissionMode::ReadOnly))
            },
        );
        tokio::pin!(driver);

        // Act
        if scenario != "already-canceled" {
            let paused = tokio::select! {
                biased;
                () = &mut driver => false,
                () = future::ready(()) => true,
            };
            assert!(paused, "scripted readiness must reach the driver's wait");
            assert_eq!(checks.load(Ordering::Relaxed), 0);
            if scenario == "resume" {
                app.services.wake_graphs();
            } else {
                cancellation.cancel();
            }
        }
        driver.await;
        app.services.wait_for_cleanup_tasks(None).await;

        // Assert
        assert_eq!(
            checks.load(Ordering::Relaxed),
            usize::from(scenario == "resume")
        );
    }
}

async fn app_with_pool() -> (App, SqlitePool, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("app directory");
    let database = Database::in_memory().await.expect("database");
    let pool = database.pool().clone();
    let mut app = App::new_with_clients(
        directory.path().into(),
        directory.path().into(),
        None,
        database,
        test_app_clients(),
    )
    .await
    .expect("app");
    seed_session(&mut app.sessions, app.services.db()).await;

    (app, pool, directory)
}

struct UnsupportedRunner {
    checks: Arc<AtomicUsize>,
}

#[async_trait]
impl GraphNodeRunner for UnsupportedRunner {
    fn supports(&self, _node: &GraphNode) -> bool {
        self.checks.fetch_add(1, Ordering::Relaxed);

        false
    }

    async fn run(&self, _request: GraphRunRequest) -> Result<String, OneShotError> {
        Err(OneShotError::new("unsupported runner cannot execute"))
    }
}
