use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ag_contracts::{
    AgentRequestKind, OneShotClient, OneShotError, OneShotRequest, OneShotSubmission,
    PermissionMode, ReasoningLevel, SessionStats, SpeedMode,
};
use ag_protocol::{AgentResponse, ProtocolRequestProfile};
use ag_session::{
    EnvId, ExecutionGraph, ExecutionGraphStatus, GraphCompletionGate, GraphExecutionRef,
    GraphWorkspaceAccess,
};
use ag_worker::{GraphInputValue, GraphNodeRunner, GraphRunRequest, MockRunClient};

use super::test_support::{ReviewGraphFixture, SESSION_ID};
use super::{ReviewNodeRunner, ReviewPhase, ReviewState};
use crate::infra::review_deadline::ReviewDeadlineClient;

fn request() -> OneShotRequest {
    OneShotRequest {
        activity_tx: None,
        child_pid: None,
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        folder: PathBuf::from("."),
        harness: "claude".into(),
        model: "claude-sonnet-5-5".into(),
        permission_mode: PermissionMode::ReadOnly,
        prompt: String::new(),
        provider_call_budget: None,
        reasoning_level: ReasoningLevel::Medium,
        request_kind: AgentRequestKind::FocusedReview,
        speed_mode: SpeedMode::Normal,
    }
}

fn empty_review() -> OneShotSubmission {
    OneShotSubmission {
        response: AgentResponse::plain(r#"{"project_impact":[],"suggestions":[]}"#),
        stats: SessionStats::default(),
    }
}

/// Diff larger than one prompt, so discovery runs as several fragment nodes.
fn fragmented_diff() -> String {
    format!(
        "diff --git a/src/lib.rs b/src/lib.rs\n{}",
        "+change\n".repeat(20_000)
    )
}

fn render() -> impl Fn(&str, &str, &str) -> Result<String, OneShotError> + Send + Sync + 'static {
    |diff, history, _| Ok(format!("{history}\n{diff}"))
}

/// Counts every child submission and returns an empty review.
fn counting_client(calls: &Arc<AtomicUsize>) -> MockRunClient {
    let observed = calls.clone();
    let mut client = MockRunClient::new();
    client.expect_submit().returning(move |request| {
        observed.fetch_add(1, Ordering::SeqCst);
        assert!(request.permission_mode.is_read_only());

        Ok(empty_review())
    });

    client
}

async fn only_graph(fixture: &ReviewGraphFixture) -> ExecutionGraph {
    let mut graphs = fixture
        .database
        .graphs()
        .list_graphs(SESSION_ID)
        .await
        .expect("graphs");
    assert_eq!(graphs.len(), 1);

    graphs.remove(0)
}

#[tokio::test]
async fn reviews_run_as_parallel_fragment_readers_joined_by_cross_file_and_consolidation() {
    // Arrange
    let fixture = ReviewGraphFixture::new().await;
    let calls = Arc::new(AtomicUsize::new(0));

    // Act
    let text = fixture
        .review(
            counting_client(&calls),
            request(),
            &fragmented_diff(),
            "",
            render(),
            |_| {},
        )
        .await
        .expect("complete review");

    // Assert
    assert!(!text.contains("Partial review"));
    let graph = only_graph(&fixture).await;
    let (batches, joins) = graph.nodes.split_at(graph.nodes.len() - 2);
    let batch_ids: Vec<_> = batches.iter().map(|node| node.id.clone()).collect();
    assert!(batch_ids.len() > 1, "discovery fans out across fragments");
    for (index, batch) in batches.iter().enumerate() {
        assert_eq!(batch.id, format!("batch-{index}"));
        assert_eq!(batch.dependencies, []);
        assert_eq!(
            batch.request.text,
            format!(
                "Review captured diff fragment {} of {}.",
                index + 1,
                batches.len()
            )
        );
    }
    let (cross_file, consolidation) = (&joins[0], &joins[1]);
    assert_eq!(cross_file.id, "cross-file");
    let producers = |node: &ag_session::GraphNode| {
        node.inputs
            .iter()
            .map(|input| input.producer_node_id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(producers(cross_file), batch_ids);
    let mut all = batch_ids.clone();
    all.push("cross-file".into());
    assert_eq!(consolidation.id, "consolidation");
    assert_eq!(producers(consolidation), all);
    assert!(graph.nodes.iter().all(|node| {
        node.execution.workspace_access == GraphWorkspaceAccess::BackgroundRead
            && node.execution.response_profile == ProtocolRequestProfile::FocusedReview
            && node.inputs.len() == node.dependencies.len()
            && node.dependencies.iter().all(|dependency| {
                dependency.gate
                    == GraphCompletionGate::ResultValidated {
                        output_name: "review".into(),
                    }
            })
    }));
    let progress = fixture
        .database
        .graphs()
        .progress(&graph.id)
        .await
        .expect("progress")
        .expect("stored graph");
    assert_eq!(progress.status, ExecutionGraphStatus::Completed);
    assert!(progress.nodes.iter().all(|node| node.executions.len() == 1));
    assert!(calls.load(Ordering::SeqCst) > batch_ids.len() * 2);
}

#[tokio::test]
async fn failed_discovery_keeps_findings_and_closes_the_generation_graph() {
    // Arrange
    let fixture = ReviewGraphFixture::new().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut client = MockRunClient::new();
    client.expect_submit().returning(move |_| {
        if observed.fetch_add(1, Ordering::SeqCst) == 1 {
            return Err(OneShotError::new("batch unavailable"));
        }

        Ok(OneShotSubmission {
            response: AgentResponse::plain(
                r#"{"project_impact":[],"suggestions":[{"severity":"high","details":"Kept finding."}]}"#,
            ),
            stats: SessionStats::default(),
        })
    });

    // Act
    let text = fixture
        .review(client, request(), &fragmented_diff(), "", render(), |_| {})
        .await
        .expect("partial review");

    // Assert
    assert!(text.contains("Kept finding."));
    assert!(text.contains("batch unavailable"));
    assert!(text.contains("Partial review:"));
    let graph = only_graph(&fixture).await;
    let progress = fixture
        .database
        .graphs()
        .progress(&graph.id)
        .await
        .expect("progress")
        .expect("stored graph");
    assert_eq!(progress.status, ExecutionGraphStatus::Cancelled);
}

#[tokio::test]
async fn graph_persistence_failures_end_the_review_without_provider_work() {
    // Arrange: the owning session disappears before its graph is saved.
    let fixture = ReviewGraphFixture::new().await;
    fixture
        .database
        .sessions()
        .delete_session(SESSION_ID)
        .await
        .expect("remove session");
    let mut client = MockRunClient::new();
    client.expect_submit().never();

    // Act
    let result = fixture
        .review(client, request(), "+change\n", "", render(), |_| {})
        .await;

    // Assert
    assert!(result.is_err());
    let graphs = fixture
        .database
        .graphs()
        .list_graphs(SESSION_ID)
        .await
        .expect("graphs");
    assert_eq!(graphs, []);
}

#[tokio::test]
async fn reviews_overlap_ordinary_session_work_but_wait_for_graph_writers() {
    // Arrange
    let fixture = Arc::new(ReviewGraphFixture::new().await);
    let env_id = EnvId::SessionWorktree(SESSION_ID.into());
    let session_work = fixture
        .host
        .resources
        .environment(&env_id)
        .write_owned()
        .await;
    let calls = Arc::new(AtomicUsize::new(0));

    // Act: ordinary session work such as a turn or sync holds the Env writer.
    let overlapping = fixture
        .review(
            counting_client(&calls),
            request(),
            "+change\n",
            "",
            render(),
            |_| {},
        )
        .await;
    let graph_writer = fixture
        .host
        .resources
        .background_readers(&env_id)
        .write_owned()
        .await;
    let blocked_calls = Arc::new(AtomicUsize::new(0));
    let blocked = tokio::spawn({
        let fixture = fixture.clone();
        let client = counting_client(&blocked_calls);
        async move {
            fixture
                .review(client, request(), "+change\n", "", render(), |_| {})
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let waited = blocked_calls.load(Ordering::SeqCst) == 0 && !blocked.is_finished();
    drop(graph_writer);
    let released = blocked.await.expect("review task");

    // Assert
    assert!(overlapping.is_ok());
    assert!(waited, "graph writers exclude background review readers");
    assert!(released.is_ok());
    drop(session_work);
}

#[tokio::test]
async fn canceling_a_waiting_review_graph_ends_the_review_without_provider_work() {
    // Arrange: a graph writer takes the Env as soon as the graph is saved.
    let fixture = Arc::new(ReviewGraphFixture::new().await);
    let gate = fixture
        .host
        .resources
        .background_readers(&EnvId::SessionWorktree(SESSION_ID.into()));
    let writer = Arc::new(std::sync::Mutex::new(None));
    let saved = Arc::new(tokio::sync::Notify::new());
    let progress = {
        let (writer, saved) = (writer.clone(), saved.clone());
        move |_| {
            let mut writer = writer.lock().expect("writer");
            if writer.is_none() {
                *writer = gate.clone().try_write_owned().ok();
                saved.notify_one();
            }
        }
    };
    let mut client = MockRunClient::new();
    client.expect_submit().never();
    let review = tokio::spawn({
        let fixture = fixture.clone();
        async move {
            fixture
                .review(client, request(), "+change\n", "", render(), progress)
                .await
        }
    });
    saved.notified().await;
    let graph = only_graph(&fixture).await;

    // Act
    fixture
        .database
        .graphs()
        .cancel(&ag_session::GraphControlRequest {
            graph_id: graph.id.clone(),
            revision: graph.revision,
        })
        .await
        .expect("cancel");
    assert!(writer.lock().expect("writer").take().is_some());
    fixture
        .host
        .wake
        .send_modify(|version| *version = version.wrapping_add(1));
    let result = tokio::time::timeout(Duration::from_secs(5), review)
        .await
        .expect("review stops")
        .expect("review task");

    // Assert
    assert_eq!(
        result,
        Err(OneShotError::new(
            "Focused review graph was canceled or is unavailable"
        ))
    );
}

#[tokio::test]
async fn the_review_deadline_bounds_env_and_dispatch_waits() {
    for wait in ["preparation", "dispatch"] {
        // Arrange: a graph writer holds the Env before preparation, or takes
        // it as soon as the graph is saved.
        let fixture = ReviewGraphFixture::new()
            .await
            .with_deadline(Duration::from_millis(200));
        let gate = fixture
            .host
            .resources
            .background_readers(&EnvId::SessionWorktree(SESSION_ID.into()));
        let held = Arc::new(std::sync::Mutex::new(if wait == "preparation" {
            Some(gate.clone().try_write_owned().expect("writer"))
        } else {
            None
        }));
        let progress = {
            let held = held.clone();
            move |_| {
                let mut held = held.lock().expect("writer");
                if held.is_none() {
                    *held = gate.clone().try_write_owned().ok();
                }
            }
        };
        let mut client = MockRunClient::new();
        client.expect_submit().never();

        // Act
        let result = fixture
            .review(client, request(), "+change\n", "", render(), progress)
            .await;

        // Assert
        assert_eq!(
            result,
            Err(OneShotError::new(
                "Focused review deadline exceeded. Press `f` to retry."
            ))
        );
        assert!(held.lock().expect("writer").is_some(), "{wait}");
    }
}

#[tokio::test]
async fn expired_history_summaries_keep_the_env_lease_through_worker_cleanup() {
    // Arrange: oversized history forces summarization, which the deadline
    // drops while the worker still awaits the runtime's cleanup.
    let fixture = ReviewGraphFixture::new().await;
    let runtime = Arc::new(CleanupRuntime::default());
    let worker = fixture.utility_worker(runtime.clone());
    let client = ReviewDeadlineClient::new(worker.clone(), Duration::from_millis(100));
    let gate = fixture
        .host
        .resources
        .background_readers(&EnvId::SessionWorktree(SESSION_ID.into()));
    let history = "history\n".repeat(10_000);

    // Act
    let result = fixture
        .review(client, request(), "+change\n", &history, render(), |_| {})
        .await;
    runtime.cleaning.cancelled().await;

    // Assert
    assert_eq!(result, Err(ReviewDeadlineClient::expired()));
    assert!(
        gate.try_write().is_err(),
        "graph writers stay excluded during cleanup"
    );
    runtime.finish.cancel();
    worker.shutdown().await;
    assert!(gate.try_write().is_ok());
}

/// Waits for cancellation, then holds its cleanup until `finish`.
#[derive(Default)]
struct CleanupRuntime {
    cleaning: tokio_util::sync::CancellationToken,
    finish: tokio_util::sync::CancellationToken,
}

#[async_trait::async_trait]
impl OneShotClient for CleanupRuntime {
    async fn submit(&self, _: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        std::future::pending().await
    }

    async fn submit_cancellable(
        &self,
        _: OneShotRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<OneShotSubmission, OneShotError> {
        cancellation.cancelled().await;
        self.cleaning.cancel();
        self.finish.cancelled().await;

        Err(OneShotError::new("stopped"))
    }
}

#[tokio::test]
async fn lifecycle_cancellation_stops_running_review_calls() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_test_app().await;
    let project = app
        .services
        .db()
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    app.services
        .db()
        .sessions()
        .insert_session(SESSION_ID, "model", "main", "Review", project)
        .await
        .expect("session");
    let started = tokio_util::sync::CancellationToken::new();
    let dropped = tokio_util::sync::CancellationToken::new();
    let client = PendingClient {
        dropped: dropped.clone(),
        started: started.clone(),
    };
    let services = app.services.clone();
    let review = tokio::spawn(async move {
        services
            .review_graph_host()
            .submit(
                Arc::new(client),
                super::ReviewGraphRequest {
                    agent: ag_session::AgentSelection::new(
                        ag_session::AgentKind::Claude,
                        ag_session::AgentModel::ClaudeSonnet55,
                    ),
                    context: String::new(),
                    deadline: tokio::time::Instant::now() + Duration::from_secs(60),
                    diff: "+change\n".into(),
                    graph_id: "focused-review:canceled".into(),
                    request: request(),
                    session_id: SESSION_ID.into(),
                },
                Arc::new(render()),
                Arc::new(|_| {}),
            )
            .await
    });
    started.cancelled().await;

    // Act
    app.cancel_api_execution_graph(ag_session::GraphControlRequest {
        graph_id: "focused-review:canceled".into(),
        revision: std::num::NonZeroU64::MIN,
    })
    .await
    .expect("cancel");
    let result = tokio::time::timeout(Duration::from_secs(5), review)
        .await
        .expect("review stops")
        .expect("review task");

    // Assert
    assert!(
        dropped.is_cancelled(),
        "the running provider call is dropped"
    );
    assert_eq!(
        result,
        Err(OneShotError::new("[Stopped] Agent run canceled")),
        "an interrupted fragment stays unfinished instead of counting as reviewed"
    );
    assert!(
        app.services
            .graph_drivers
            .lock()
            .expect("drivers")
            .is_empty()
    );
}

/// Signals its first call, then waits until dropped.
struct PendingClient {
    dropped: tokio_util::sync::CancellationToken,
    started: tokio_util::sync::CancellationToken,
}

#[async_trait::async_trait]
impl ag_worker::RunClient for PendingClient {
    async fn submit(&self, _: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        let _dropped = self.dropped.clone().drop_guard();
        self.started.cancel();

        std::future::pending().await
    }
}

#[tokio::test]
async fn provider_calls_stay_within_three_across_subdividing_nodes() {
    // Arrange: every original fragment is too large for the provider, so
    // concurrent discovery nodes each subdivide their fragment.
    let fixture = ReviewGraphFixture::new().await;
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (observed_active, observed_peak) = (active.clone(), peak.clone());
    let client = LimitedClient {
        active: observed_active,
        peak: observed_peak,
    };

    // Act
    let text = fixture
        .review(client, request(), &fragmented_diff(), "", render(), |_| {})
        .await
        .expect("complete review");

    // Assert
    assert!(!text.contains("Partial review"), "{text}");
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert_eq!(peak.load(Ordering::SeqCst), 3);
}

/// Rejects prompts above a smaller provider limit and records concurrency.
struct LimitedClient {
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ag_worker::RunClient for LimitedClient {
    async fn submit(&self, request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        let running = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(running, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        if request.request_kind == AgentRequestKind::UtilityPrompt {
            return Ok(OneShotSubmission {
                response: AgentResponse::plain("Shared overview."),
                stats: SessionStats::default(),
            });
        }
        if request.prompt.len() > 30_000 {
            return Err(OneShotError::new("contextWindowExceeded"));
        }

        Ok(empty_review())
    }
}

#[tokio::test]
async fn review_nodes_reject_foreign_profiles_and_task_attempts() {
    // Arrange
    let fixture = ReviewGraphFixture::new().await;
    let calls = Arc::new(AtomicUsize::new(0));
    fixture
        .review(
            counting_client(&calls),
            request(),
            "+change\n",
            "",
            render(),
            |_| {},
        )
        .await
        .expect("review");
    let graph = only_graph(&fixture).await;
    let mut node = graph.nodes[0].clone();
    let runner = runner(&fixture);
    node.execution.workspace_access = GraphWorkspaceAccess::SharedRead;
    let shared_reader = runner.supports(&node);
    let dispatch = task_dispatch(&graph);

    // Act
    let result = runner
        .run(GraphRunRequest {
            dispatch,
            folder: PathBuf::from("."),
            permission_mode: PermissionMode::ReadOnly,
            provider_call_budget: None,
        })
        .await;

    // Assert
    assert!(runner.supports(&graph.nodes[0]));
    assert!(!shared_reader);
    assert_eq!(
        result,
        Err(OneShotError::new("Review node requires a utility run"))
    );
}

#[tokio::test]
async fn malformed_pinned_inputs_fail_the_join_without_provider_work() {
    // Arrange
    let fixture = ReviewGraphFixture::new().await;
    let calls = Arc::new(AtomicUsize::new(0));
    fixture
        .review(
            counting_client(&calls),
            request(),
            "+change\n",
            "",
            render(),
            |_| {},
        )
        .await
        .expect("review");
    let graph = only_graph(&fixture).await;
    let mut dispatch = task_dispatch(&graph);
    dispatch.node = graph.nodes[1].clone();
    let reference = fixture
        .database
        .graphs()
        .list_results(&graph.id, "batch-0", "review")
        .await
        .expect("results")
        .remove(0);
    dispatch.inputs = vec![GraphInputValue {
        body: "not a review".into(),
        reference: graph
            .bind_input("cross-file", "batch-0", &reference.result)
            .expect("binding"),
    }];
    let mut client = MockRunClient::new();
    client.expect_submit().never();
    let phase = ReviewPhase {
        budget: ag_contracts::ProviderCallBudget::new(1),
        client: Arc::new(client),
        diff: "+change\n".into(),
        dispatch,
        fragment: None,
        progress: Arc::new(|_| {}),
        render: Arc::new(render()),
        request: request(),
        state: Arc::new(ReviewState::new(String::new(), 1, Arc::new(|_| {}))),
    };

    // Act
    let result = phase.run().await;

    // Assert
    assert!(result.is_err());
}

fn runner(fixture: &ReviewGraphFixture) -> ReviewNodeRunner {
    ReviewNodeRunner {
        budget: ag_contracts::ProviderCallBudget::new(1),
        client: Arc::new(MockRunClient::new()),
        diff: "+change\n".into(),
        leaves: std::collections::HashMap::new(),
        progress: Arc::new(|_| {}),
        render: Arc::new(render()),
        request: request(),
        runs: fixture.host.runs.clone(),
        state: Arc::new(ReviewState::new(String::new(), 1, Arc::new(|_| {}))),
    }
}

/// A receipt for a task attempt, which review nodes never execute.
fn task_dispatch(graph: &ExecutionGraph) -> ag_worker::GraphDispatch {
    ag_worker::GraphDispatch {
        inputs: Vec::new(),
        node: graph.nodes[0].clone(),
        record: ag_worker::GraphExecutionRecord {
            execution: GraphExecutionRef::TaskAttempt {
                attempt_id: "attempt".into(),
                operation_id: "operation".into(),
                task_id: "task".into(),
            },
            graph_id: graph.id.clone(),
            graph_revision: graph.revision,
            inputs: Vec::new(),
            node_id: graph.nodes[0].id.clone(),
        },
    }
}
