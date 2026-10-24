use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ag_contracts::OneShotError;
use ag_protocol::ProtocolRequestProfile;
use ag_session::{
    ExecutionGraph, GraphContext, GraphExecutionRef, GraphNode, GraphPostProcessing,
    StartExecutionGraphRequest,
};
use ag_worker::{
    GraphAdmission, GraphDispatch, GraphDispatchRepository, GraphExecutionRecord, GraphExecutor,
    GraphNodeRunner, GraphRunRequest, RunRepository, RunState,
};
use async_trait::async_trait;
use tokio::sync::{Notify, mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::{graph, seed_session};
use crate::app::AppServices;

#[tokio::test]
async fn waiting_graph_reuses_released_capacity_before_another_wave_finishes() {
    for fast_fails in [false, true] {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_test_app().await;
        seed_session(&mut app.sessions, app.services.db()).await;
        let first = utility_graph(
            "busy",
            &[
                "fast", "slow-1", "slow-2", "slow-3", "slow-4", "slow-5", "slow-6", "slow-7",
            ],
        );
        let second = utility_graph("waiting", &["waiting"]);
        let (started, mut started_rx) = mpsc::unbounded_channel();
        let runner = Arc::new(StaggeredRunner {
            fast_fails,
            fast_release: Notify::new(),
            runs: app.services.db().runs(),
            slow_release: Notify::new(),
            started,
        });
        let (scanned, mut scanned_rx) = mpsc::unbounded_channel();
        for snapshot in [first, second] {
            app.services
                .db()
                .graphs()
                .save_graph(&snapshot, None)
                .await
                .expect("save graph");
            let repository = Arc::new(ObservedRepository {
                inner: app.services.db().graph_dispatch(),
                scanned: scanned.clone(),
            });
            spawn_driver(&app.services, snapshot.clone(), repository, runner.clone());
            if snapshot.id == "busy" {
                for _ in 0..8 {
                    assert_ne!(
                        started_rx.recv().await.expect("busy node started"),
                        "waiting"
                    );
                }
            }
        }
        loop {
            if scanned_rx.recv().await.expect("driver scan") == "waiting" {
                break;
            }
        }
        assert!(
            started_rx.try_recv().is_err(),
            "all eight slots are occupied"
        );

        // Act: only the fast utility settles; seven unrelated peers stay
        // blocked.
        runner.fast_release.notify_one();
        let next = tokio::time::timeout(Duration::from_secs(5), started_rx.recv())
            .await
            .expect("released capacity wakes the waiting graph")
            .expect("waiting node starts");

        // Assert
        assert_eq!(next, "waiting");
        runner.slow_release.notify_waiters();
        app.services.wait_for_cleanup_tasks(None).await;
    }
}

#[tokio::test]
async fn unclaimed_checks_wake_a_capacity_blocked_driver_without_external_events() {
    for existing in [false, true] {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_test_app().await;
        seed_session(&mut app.sessions, app.services.db()).await;
        let (entered, mut entered_rx) = mpsc::unbounded_channel();
        let (release, released) = watch::channel(false);
        let repository = Arc::new(UnclaimedRepository {
            entered,
            existing,
            inner: app.services.db().graph_dispatch(),
            released,
        });
        let (started, mut started_rx) = mpsc::unbounded_channel();
        let runner = Arc::new(StaggeredRunner {
            fast_fails: false,
            fast_release: Notify::new(),
            runs: app.services.db().runs(),
            slow_release: Notify::new(),
            started,
        });
        for index in 0..8 {
            let snapshot = utility_graph(&format!("checking-{index}"), &["check"]);
            app.services
                .db()
                .graphs()
                .save_graph(&snapshot, None)
                .await
                .expect("save checker");
            spawn_driver(&app.services, snapshot, repository.clone(), runner.clone());
        }
        for _ in 0..8 {
            entered_rx.recv().await.expect("check holds a permit");
        }
        let snapshot = utility_graph("waiting", &["waiting"]);
        app.services
            .db()
            .graphs()
            .save_graph(&snapshot, None)
            .await
            .expect("save waiting graph");
        let (scanned, mut scanned_rx) = mpsc::unbounded_channel();
        spawn_driver(
            &app.services,
            snapshot,
            Arc::new(ObservedRepository {
                inner: app.services.db().graph_dispatch(),
                scanned,
            }),
            runner,
        );
        assert_eq!(scanned_rx.recv().await.as_deref(), Some("waiting"));
        assert!(started_rx.try_recv().is_err());

        // Act: none of the eight checks admits a node or broadcasts settlement.
        release.send_replace(true);
        let started = tokio::time::timeout(Duration::from_secs(5), started_rx.recv())
            .await
            .expect("unclaimed permit returns wake the waiting driver");

        // Assert
        assert_eq!(started.as_deref(), Some("waiting"));
        app.services.wait_for_cleanup_tasks(None).await;
    }
}

fn spawn_driver(
    services: &AppServices,
    snapshot: ExecutionGraph,
    repository: Arc<dyn GraphDispatchRepository>,
    runner: Arc<dyn GraphNodeRunner>,
) {
    let executor = GraphExecutor::new(
        repository,
        Arc::new(services.graph_resources.with_budgets(HashMap::new())),
        runner,
    );
    services.spawn_graph_driver(
        StartExecutionGraphRequest {
            graph: snapshot,
            provider_call_limits: BTreeMap::new(),
        },
        CancellationToken::new(),
        executor,
        "workspace".into(),
    );
}

fn utility_graph(id: &str, nodes: &[&str]) -> ExecutionGraph {
    let mut graph = graph();
    let mut template = graph.nodes.remove(0);
    template.execution.context = GraphContext::Fresh;
    template.execution.post_processing = GraphPostProcessing::None;
    template.execution.response_profile = ProtocolRequestProfile::UtilityPrompt;
    template.execution.shared_budget_id = None;
    template.request.attachments.clear();
    template.outputs.clear();
    graph.id = id.into();
    graph.nodes = nodes
        .iter()
        .map(|id| {
            let mut node = template.clone();
            node.id = (*id).into();

            node
        })
        .collect();

    graph
}

struct StaggeredRunner {
    fast_fails: bool,
    fast_release: Notify,
    runs: Arc<dyn RunRepository>,
    slow_release: Notify,
    started: mpsc::UnboundedSender<String>,
}

#[async_trait]
impl GraphNodeRunner for StaggeredRunner {
    fn supports(&self, _node: &GraphNode) -> bool {
        true
    }

    async fn run(&self, request: GraphRunRequest) -> Result<String, OneShotError> {
        let id = request.dispatch.node.id;
        self.started.send(id.clone()).expect("observe start");
        if id == "fast" {
            self.fast_release.notified().await;
            if self.fast_fails {
                return Err(OneShotError::new("fast utility failed"));
            }
        } else if id != "waiting" {
            self.slow_release.notified().await;
        }
        let GraphExecutionRef::UtilityRun { run_id } = request.dispatch.record.execution else {
            return Err(OneShotError::new("expected utility run"));
        };
        self.runs
            .transition(&run_id, RunState::Completed, None)
            .await?;

        Ok(serde_json::json!({"answer": "done"}).to_string())
    }
}

struct ObservedRepository {
    inner: Arc<dyn GraphDispatchRepository>,
    scanned: mpsc::UnboundedSender<String>,
}

#[async_trait]
impl GraphDispatchRepository for ObservedRepository {
    async fn graph(&self, id: &str, revision: NonZeroU64) -> Result<ExecutionGraph, OneShotError> {
        let graph = self.inner.graph(id, revision).await?;
        // Sending and returning do not yield: a full-capacity scan then reaches
        // the driver's wake wait before the test can release a busy node.
        self.scanned.send(id.into()).expect("observe scan");

        Ok(graph)
    }

    async fn claim(
        &self,
        graph_id: &str,
        revision: NonZeroU64,
        node_id: &str,
        execution_id: &str,
        folder: &Path,
    ) -> Result<GraphAdmission, OneShotError> {
        self.inner
            .claim(graph_id, revision, node_id, execution_id, folder)
            .await
    }

    async fn finish(&self, dispatch: &GraphDispatch, body: &str) -> Result<(), OneShotError> {
        self.inner.finish(dispatch, body).await
    }

    async fn fail(&self, record: &GraphExecutionRecord, error: &str) -> Result<(), OneShotError> {
        self.inner.fail(record, error).await
    }
}

struct UnclaimedRepository {
    entered: mpsc::UnboundedSender<()>,
    existing: bool,
    inner: Arc<dyn GraphDispatchRepository>,
    released: watch::Receiver<bool>,
}

#[async_trait]
impl GraphDispatchRepository for UnclaimedRepository {
    async fn graph(&self, id: &str, revision: NonZeroU64) -> Result<ExecutionGraph, OneShotError> {
        self.inner.graph(id, revision).await
    }

    async fn claim(
        &self,
        graph_id: &str,
        revision: NonZeroU64,
        node_id: &str,
        _execution_id: &str,
        _folder: &Path,
    ) -> Result<GraphAdmission, OneShotError> {
        self.entered.send(()).expect("observe claim check");
        let mut released = self.released.clone();
        released
            .wait_for(|released| *released)
            .await
            .expect("release checks");

        Ok(if self.existing {
            GraphAdmission::Existing(GraphExecutionRecord {
                execution: GraphExecutionRef::UtilityRun {
                    run_id: "existing-run".into(),
                },
                graph_id: graph_id.into(),
                graph_revision: revision,
                inputs: Vec::new(),
                node_id: node_id.into(),
            })
        } else {
            GraphAdmission::Blocked
        })
    }

    async fn finish(&self, _dispatch: &GraphDispatch, _body: &str) -> Result<(), OneShotError> {
        Err(OneShotError::new("unclaimed checks cannot finish"))
    }

    async fn fail(&self, _record: &GraphExecutionRecord, _error: &str) -> Result<(), OneShotError> {
        Err(OneShotError::new("unclaimed checks cannot settle"))
    }
}
