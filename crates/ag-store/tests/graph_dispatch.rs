//! Static graph execution through durable admission and the real utility
//! worker.

#[path = "graph_dispatch/support.rs"]
mod support;

use std::collections::HashMap;
use std::error::Error;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::Path;
use std::sync::Arc;

use ag_contracts::{PermissionMode, ProviderCallBudget};
use ag_protocol::ProtocolRequestProfile;
use ag_session::{GraphContext, GraphResultState, GraphWorkspaceAccess};
use ag_store::{Database, GraphEvidence, TaskAgentTurnMetadata};
use ag_worker::{
    GraphAdmission, GraphExecutor, GraphResources, GraphUtilityRunner, HeartbeatClock, RunScope,
    RunWorker, in_scope,
};
use support::{
    BusyProvider, CleanupProvider, PanickingRunner, PausedAdmission, Provider, ScannedAdmission,
    assert_panicked_execution_failed, graph, populate,
};
use tokio::sync::Notify;

#[tokio::test]
async fn graph_task_history_is_immutable_and_accounts_usage_before_effects()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let TaskHistoryFixture {
        database,
        dispatch,
        graph,
        metadata,
    } = task_history_fixture().await?;

    // Act / Assert: a missing receipt cannot create observed task history.
    let mut missing = dispatch.clone();
    missing.record.execution = ag_session::GraphExecutionRef::UtilityRun {
        run_id: "missing".into(),
    };
    assert!(
        database
            .graphs()
            .record_task_turn(&missing, &metadata)
            .await
            .is_err()
    );
    assert_task_turn_admission(&database, &dispatch, &metadata).await?;

    // Act: effects fail after the observed response was saved.
    database
        .graph_dispatch()
        .fail(&dispatch.record, "effects failed")
        .await?;
    let retained = database
        .graphs()
        .load_task_turn(&dispatch.record.execution)
        .await?;
    let usage: (i64, i64) = sqlx::query_as(
        "SELECT input_tokens, output_tokens FROM session_usage WHERE session_id = 'session'",
    )
    .fetch_one(database.pool())
    .await?;

    // Assert
    assert_eq!(retained, Some(metadata.clone()));
    assert_eq!(usage, (3, 7));
    assert_eq!(
        database
            .graphs()
            .list_results("graph", "first", "response")
            .await?
            .len(),
        0
    );
    assert_eq!(
        database
            .sessions()
            .load_session_messages("session")
            .await?
            .len(),
        0
    );
    sqlx::query("UPDATE execution_graph_task_turn SET metadata = 'invalid'")
        .execute(database.pool())
        .await?;
    assert!(
        database
            .graphs()
            .load_task_turn(&dispatch.record.execution)
            .await
            .is_err()
    );
    let mut utility = graph;
    utility.id = "utility-graph".into();
    utility.nodes[0].execution.context = GraphContext::Fresh;
    utility.nodes[0].execution.response_profile = ProtocolRequestProfile::UtilityPrompt;
    utility.nodes[0].outputs[0].schema = ProtocolRequestProfile::UtilityPrompt;
    database.graphs().save_graph(&utility, None).await?;
    let GraphAdmission::Claimed(utility) = database
        .graph_dispatch()
        .claim(
            "utility-graph",
            utility.revision,
            "first",
            "utility",
            Path::new("workspace"),
        )
        .await?
    else {
        return Err("utility was not admitted".into());
    };
    assert!(
        database
            .graphs()
            .record_task_turn(&utility, &metadata)
            .await
            .is_err()
    );

    Ok(())
}

struct TaskHistoryFixture {
    database: Database,
    dispatch: ag_worker::GraphDispatch,
    graph: ag_session::ExecutionGraph,
    metadata: TaskAgentTurnMetadata,
}

async fn task_history_fixture() -> Result<TaskHistoryFixture, Box<dyn Error>> {
    let database = Database::open_in_memory().await?;
    populate(&database).await?;
    let mut graph = graph();
    graph.nodes.truncate(1);
    graph.nodes[0].execution.context = GraphContext::TaskConversation {
        task_id: "task".into(),
    };
    graph.nodes[0].execution.response_profile = ProtocolRequestProfile::SessionTurn;
    graph.nodes[0].outputs[0].schema = ProtocolRequestProfile::SessionTurn;
    database.graphs().save_graph(&graph, None).await?;
    let GraphAdmission::Claimed(dispatch) = database
        .graph_dispatch()
        .claim(
            "graph",
            graph.revision,
            "first",
            "attempt",
            Path::new("workspace"),
        )
        .await?
    else {
        return Err("task was not admitted".into());
    };
    let metadata = TaskAgentTurnMetadata {
        assistant_message: serde_json::json!(ag_protocol::AgentResponse::plain("response"))
            .to_string(),
        harness_turn_report: None,
        instruction_conversation_id: Some("task-instructions".into()),
        input_tokens: 3,
        output_tokens: 7,
        provider_conversation_id: Some("task-provider".into()),
    };

    Ok(TaskHistoryFixture {
        database,
        dispatch: *dispatch,
        graph,
        metadata,
    })
}

async fn assert_task_turn_admission(
    database: &Database,
    dispatch: &ag_worker::GraphDispatch,
    metadata: &TaskAgentTurnMetadata,
) -> Result<(), Box<dyn Error>> {
    assert_eq!(
        database.graphs().list_task_context_ids("session").await?,
        ["attempt"]
    );
    assert_eq!(
        database
            .graphs()
            .list_task_context_ids("other-session")
            .await?
            .len(),
        0
    );

    // Act / Assert: only matching running claims can record a response.
    assert_eq!(
        database
            .graphs()
            .load_task_turn(&dispatch.record.execution)
            .await?,
        None
    );
    assert_eq!(
        database
            .graphs()
            .load_task_turn(&ag_session::GraphExecutionRef::UtilityRun {
                run_id: "missing".into()
            })
            .await?,
        None
    );
    assert!(
        database
            .graphs()
            .record_task_turn(dispatch, metadata)
            .await
            .is_err()
    );
    let mut invalid = dispatch.clone();
    invalid.node.objective.push_str("changed");
    assert!(
        database
            .graphs()
            .record_task_turn(&invalid, metadata)
            .await
            .is_err()
    );
    let ag_session::GraphExecutionRef::TaskAttempt { operation_id, .. } =
        &dispatch.record.execution
    else {
        return Err("task execution expected".into());
    };
    database
        .operations()
        .mark_session_operation_running(operation_id)
        .await?;
    let mut malformed = metadata.clone();
    malformed.assistant_message = "invalid".into();
    assert!(
        database
            .graphs()
            .record_task_turn(dispatch, &malformed)
            .await
            .is_err()
    );
    assert!(
        database
            .graphs()
            .record_task_turn(dispatch, metadata)
            .await?
    );
    assert!(
        !database
            .graphs()
            .record_task_turn(dispatch, metadata)
            .await?
    );
    assert_eq!(
        database
            .graphs()
            .load_task_turn(&dispatch.record.execution)
            .await?,
        Some(metadata.clone())
    );
    let mut conflicting = metadata.clone();
    conflicting.provider_conversation_id = None;
    assert!(
        database
            .graphs()
            .record_task_turn(dispatch, &conflicting)
            .await
            .is_err()
    );

    Ok(())
}

#[tokio::test]
async fn queued_readers_reserve_calls_before_admission_and_resume_with_a_fresh_budget()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let database = Database::open_in_memory().await?;
    populate(&database).await?;
    let project = database.projects().upsert_project("project", None).await?;
    database
        .sessions()
        .insert_session("other-session", "gpt-6.1-sol", "main", "Review", project)
        .await?;
    let mut graph = graph();
    // An unbudgeted probe confirms both ready readers have been considered,
    // including a reader whose reservation fails before reaching storage.
    let mut probe = graph.nodes[0].clone();
    probe.id = "probe".into();
    probe.execution.shared_budget_id = None;
    graph.nodes.push(probe);
    database.graphs().save_graph(&graph, None).await?;
    let provider = Arc::new(BusyProvider::default());
    let worker = Arc::new(RunWorker::with_client(
        provider.clone(),
        database.runs(),
        Arc::new(HeartbeatClock),
        NonZeroUsize::MIN,
    ));
    let holder = provider.hold_capacity(worker.clone()).await;
    let admission = Arc::new(ScannedAdmission {
        scanned: Notify::new(),
        repository: database.graph_dispatch(),
    });
    let budget = ProviderCallBudget::new(1);
    let runner = Arc::new(GraphUtilityRunner::new(worker.clone()));
    let executor = Arc::new(GraphExecutor::new(
        admission.clone(),
        Arc::new(GraphResources::new(
            NonZeroUsize::new(4).expect("graph capacity"),
            HashMap::from([("budget".into(), budget.clone())]),
        )),
        runner.clone(),
    ));

    // Act
    let driver = tokio::spawn(async move {
        executor
            .run_ready(
                "graph",
                NonZeroU64::MIN,
                Path::new("workspace"),
                PermissionMode::ReadOnly,
            )
            .await
    });
    admission.scanned.notified().await;
    let queued = database
        .graphs()
        .list_executions("graph", graph.revision)
        .await?;
    let available = budget.remaining();
    let attempts = provider.provider.requests.lock().expect("requests").len();
    provider.release.notify_one();
    holder.await??;
    let result = driver.await?;

    // Assert
    assert_eq!(
        queued.len(),
        1,
        "one call can admit only one waiting reader"
    );
    assert_eq!(
        available, 0,
        "the initial attempt is reserved while capacity is busy"
    );
    assert_eq!(attempts, 0, "no graph provider attempt has started");
    assert_eq!(result?.len(), 1);

    // Act: run only the deferred reader and join with a fresh budget.
    let resumed = GraphExecutor::new(
        admission,
        Arc::new(GraphResources::new(
            NonZeroUsize::new(4).expect("graph capacity"),
            HashMap::from([("budget".into(), ProviderCallBudget::new(2))]),
        )),
        runner,
    )
    .run_ready(
        "graph",
        graph.revision,
        Path::new("workspace"),
        PermissionMode::ReadOnly,
    )
    .await?;
    worker.shutdown().await;

    // Assert
    assert_eq!(
        resumed
            .iter()
            .map(|record| record.node_id.as_str())
            .collect::<Vec<_>>(),
        ["second", "join"]
    );
    assert_eq!(
        provider.provider.requests.lock().expect("requests").len(),
        3
    );
    let statuses: Vec<String> =
        sqlx::query_scalar("SELECT status FROM agent_run WHERE session_id = 'session'")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(statuses, ["completed", "completed", "completed"]);

    Ok(())
}

#[tokio::test]
async fn exhausted_budgets_leave_nodes_unclaimed_for_a_fresh_budget() -> Result<(), Box<dyn Error>>
{
    for (limit, capacity) in [(0, 1), (1, 1), (2, 3)] {
        // Arrange
        let database = Database::open_in_memory().await?;
        populate(&database).await?;
        let graph = graph();
        database.graphs().save_graph(&graph, None).await?;
        let provider = Arc::new(Provider::default());
        let worker = Arc::new(RunWorker::with_client(
            provider.clone(),
            database.runs(),
            Arc::new(HeartbeatClock),
            NonZeroUsize::new(capacity).expect("capacity"),
        ));
        let budget = ProviderCallBudget::new(limit);
        let runner = Arc::new(GraphUtilityRunner::new(worker.clone()));
        let executor = GraphExecutor::new(
            database.graph_dispatch(),
            Arc::new(GraphResources::new(
                NonZeroUsize::new(capacity).expect("capacity"),
                HashMap::from([("budget".into(), budget.clone())]),
            )),
            runner.clone(),
        );

        // Act
        let receipts = executor
            .run_ready(
                "graph",
                graph.revision,
                Path::new("workspace"),
                PermissionMode::ReadOnly,
            )
            .await?;

        // Assert
        assert_eq!(receipts.len(), limit);
        assert_eq!(budget.remaining(), 0);
        assert_eq!(
            database
                .graphs()
                .list_executions("graph", graph.revision)
                .await?,
            receipts,
        );
        let statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM agent_run")
            .fetch_all(database.pool())
            .await?;
        assert_eq!(statuses, vec!["completed"; limit]);
        assert_eq!(provider.requests.lock().expect("requests").len(), limit);

        // Act: supply a fresh host budget without replaying admitted nodes.
        let resumed = GraphExecutor::new(
            database.graph_dispatch(),
            Arc::new(GraphResources::new(
                NonZeroUsize::new(capacity).expect("capacity"),
                HashMap::from([("budget".into(), ProviderCallBudget::new(3))]),
            )),
            runner,
        )
        .run_ready(
            "graph",
            graph.revision,
            Path::new("workspace"),
            PermissionMode::ReadOnly,
        )
        .await?;
        worker.shutdown().await;

        // Assert
        assert_eq!(resumed.len(), 3 - limit);
        assert_eq!(provider.requests.lock().expect("requests").len(), 3);
        assert_eq!(
            database
                .graphs()
                .list_executions("graph", graph.revision)
                .await?
                .len(),
            3,
        );
    }

    Ok(())
}

#[tokio::test]
async fn runner_panic_fails_the_authority_and_unblocks_conflicting_env_work()
-> Result<(), Box<dyn Error>> {
    for (task, running) in [(false, false), (false, true), (true, false), (true, true)] {
        // Arrange
        let database = Database::open_in_memory().await?;
        populate(&database).await?;
        let mut workflow = graph();
        workflow.nodes.truncate(1);
        if task {
            workflow.nodes[0].execution.context = GraphContext::TaskConversation {
                task_id: "task".into(),
            };
            workflow.nodes[0].execution.response_profile = ProtocolRequestProfile::SessionTurn;
            workflow.nodes[0].outputs[0].schema = ProtocolRequestProfile::SessionTurn;
        }
        database.graphs().save_graph(&workflow, None).await?;
        let mut follow_up = graph();
        follow_up.id = "follow-up".into();
        follow_up.nodes.truncate(1);
        follow_up.nodes[0].execution.workspace_access = GraphWorkspaceAccess::ExclusiveWrite;
        database.graphs().save_graph(&follow_up, None).await?;
        let resources = Arc::new(GraphResources::new(
            NonZeroUsize::MIN,
            HashMap::from([("budget".into(), ProviderCallBudget::new(1))]),
        ));
        let environment = resources.environment(&workflow.nodes[0].env_id);
        let runner = Arc::new(PanickingRunner {
            database: database.clone(),
            entered: Notify::new(),
            release: Notify::new(),
            running,
        });
        let executor = Arc::new(GraphExecutor::new(
            database.graph_dispatch(),
            resources,
            runner.clone(),
        ));
        let driver = {
            let executor = executor.clone();
            tokio::spawn(async move {
                executor
                    .run_ready(
                        "graph",
                        NonZeroU64::MIN,
                        Path::new("workspace"),
                        PermissionMode::ReadOnly,
                    )
                    .await
            })
        };
        runner.entered.notified().await;
        let status: String = sqlx::query_scalar(
            "SELECT status FROM agent_run UNION ALL SELECT status FROM session_operation",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(status, if running { "running" } else { "queued" });
        assert_eq!(
            database
                .graph_dispatch()
                .claim(
                    "follow-up",
                    follow_up.revision,
                    "first",
                    "premature",
                    Path::new("workspace"),
                )
                .await?,
            GraphAdmission::Blocked
        );
        assert!(environment.try_write().is_err());

        // Act
        runner.release.notify_one();
        let result = driver.await?;

        // Assert
        let error = result.expect_err("runner panic").to_string();
        assert_eq!(error, "Graph node runner panicked");
        assert_panicked_execution_failed(&database).await?;
        assert!(environment.try_write().is_ok());
        assert_eq!(
            executor
                .run_ready(
                    "graph",
                    workflow.revision,
                    Path::new("workspace"),
                    PermissionMode::ReadOnly,
                )
                .await?,
            Vec::<ag_worker::GraphExecutionRecord>::new()
        );
        assert!(matches!(
            database
                .graph_dispatch()
                .claim(
                    "follow-up",
                    follow_up.revision,
                    "first",
                    "next",
                    Path::new("workspace"),
                )
                .await?,
            GraphAdmission::Claimed(_)
        ));
    }

    Ok(())
}

#[tokio::test]
async fn admission_failure_reaches_the_driver_without_executing_work() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let database = Database::open_in_memory().await?;
    populate(&database).await?;
    let mut graph = graph();
    graph.nodes.reverse();
    database.graphs().save_graph(&graph, None).await?;
    database.runs().close_session("session").await?;
    let provider = Arc::new(Provider::default());
    let worker = Arc::new(RunWorker::with_client(
        provider.clone(),
        database.runs(),
        Arc::new(HeartbeatClock),
        NonZeroUsize::MIN,
    ));
    let executor = GraphExecutor::new(
        database.graph_dispatch(),
        Arc::new(GraphResources::new(
            NonZeroUsize::MIN,
            HashMap::from([("budget".into(), ProviderCallBudget::new(1))]),
        )),
        Arc::new(GraphUtilityRunner::new(worker.clone())),
    );

    // Act
    let result = executor
        .run_ready(
            "graph",
            graph.revision,
            Path::new("workspace"),
            PermissionMode::ReadOnly,
        )
        .await;
    worker.shutdown().await;

    // Assert
    assert!(
        result
            .expect_err("closed admission")
            .to_string()
            .contains("Session utility admission is closed")
    );
    assert_eq!(provider.requests.lock().expect("requests").len(), 0);
    assert_eq!(
        database
            .graphs()
            .list_executions("graph", graph.revision)
            .await?,
        Vec::<ag_worker::GraphExecutionRecord>::new()
    );

    Ok(())
}

#[tokio::test]
async fn serial_dispatch_scans_past_blocked_and_existing_nodes() -> Result<(), Box<dyn Error>> {
    // Arrange
    let database = Database::open_in_memory().await?;
    populate(&database).await?;
    let mut graph = graph();
    graph.nodes.reverse();
    database.graphs().save_graph(&graph, None).await?;
    let provider = Arc::new(Provider::default());
    let worker = Arc::new(RunWorker::with_client(
        provider.clone(),
        database.runs(),
        Arc::new(HeartbeatClock),
        NonZeroUsize::MIN,
    ));
    let executor = GraphExecutor::new(
        database.graph_dispatch(),
        Arc::new(GraphResources::new(
            NonZeroUsize::MIN,
            HashMap::from([("budget".into(), ProviderCallBudget::new(3))]),
        )),
        Arc::new(GraphUtilityRunner::new(worker.clone())),
    );

    // Act
    let receipts = executor
        .run_ready(
            "graph",
            graph.revision,
            Path::new("workspace"),
            PermissionMode::ReadOnly,
        )
        .await?;
    worker.shutdown().await;

    // Assert
    assert_eq!(
        receipts
            .iter()
            .map(|record| record.node_id.as_str())
            .collect::<Vec<_>>(),
        ["second", "first", "join"]
    );
    assert_eq!(provider.requests.lock().expect("requests").len(), 3);

    Ok(())
}

#[tokio::test]
async fn dropped_driver_during_committed_admission_keeps_an_owner_until_settlement()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let database = Database::open_in_memory().await?;
    populate(&database).await?;
    let mut graph = graph();
    graph.nodes.truncate(1);
    database.graphs().save_graph(&graph, None).await?;
    let provider = Arc::new(Provider::default());
    let worker = Arc::new(RunWorker::with_client(
        provider.clone(),
        database.runs(),
        Arc::new(HeartbeatClock),
        NonZeroUsize::MIN,
    ));
    let resources = Arc::new(GraphResources::new(
        NonZeroUsize::MIN,
        HashMap::from([("budget".into(), ProviderCallBudget::new(1))]),
    ));
    let environment = resources.environment(&graph.nodes[0].env_id);
    let admission = Arc::new(PausedAdmission::new(database.graph_dispatch()));
    let executor = Arc::new(GraphExecutor::new(
        admission.clone(),
        resources,
        Arc::new(GraphUtilityRunner::new(worker.clone())),
    ));
    let driver = {
        let executor = executor.clone();
        tokio::spawn(async move {
            executor
                .run_ready(
                    "graph",
                    NonZeroU64::MIN,
                    Path::new("workspace"),
                    PermissionMode::ReadOnly,
                )
                .await
        })
    };
    admission.committed.notified().await;
    let queued: String = sqlx::query_scalar("SELECT status FROM agent_run")
        .fetch_one(database.pool())
        .await?;

    // Act
    driver.abort();
    assert!(driver.await.expect_err("driver aborted").is_cancelled());

    // Assert
    assert_eq!(queued, "queued");
    assert!(
        environment.try_write().is_err(),
        "admission must retain its Env lease"
    );
    assert_eq!(provider.requests.lock().expect("requests").len(), 0);
    assert_eq!(
        executor
            .run_ready(
                "graph",
                graph.revision,
                Path::new("workspace"),
                PermissionMode::ReadOnly
            )
            .await?,
        Vec::<ag_worker::GraphExecutionRecord>::new()
    );
    admission.release.notify_one();
    let settled = environment.write().await;
    let status: String = sqlx::query_scalar("SELECT status FROM agent_run")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(status, "completed");
    assert_eq!(
        database
            .graphs()
            .list_results("graph", "first", "response")
            .await?[0]
            .result
            .state,
        GraphResultState::Validated
    );
    assert_eq!(provider.requests.lock().expect("requests").len(), 1);
    drop(settled);
    assert_eq!(
        executor
            .run_ready(
                "graph",
                graph.revision,
                Path::new("workspace"),
                PermissionMode::ReadOnly
            )
            .await?,
        Vec::<ag_worker::GraphExecutionRecord>::new()
    );
    worker.shutdown().await;

    Ok(())
}

#[tokio::test]
async fn utility_profiles_keep_their_wire_schema_and_worker_default_budget()
-> Result<(), Box<dyn Error>> {
    // Arrange
    for profile in [
        ag_protocol::ProtocolRequestProfile::UtilityPrompt,
        ag_protocol::ProtocolRequestProfile::FocusedReview,
        ag_protocol::ProtocolRequestProfile::ReviewMetadata,
    ] {
        let database = Database::open_in_memory().await?;
        populate(&database).await?;
        let mut graph = graph();
        graph.nodes.truncate(1);
        graph.nodes[0].execution.response_profile = profile;
        graph.nodes[0].execution.shared_budget_id = None;
        graph.nodes[0].outputs[0].schema = profile;
        graph.nodes[0].request = ag_protocol::TurnPrompt::from_text("Inspect @file.rs".into());
        database.graphs().save_graph(&graph, None).await?;
        let provider = Arc::new(Provider::default());
        let worker = Arc::new(RunWorker::with_client(
            provider.clone(),
            database.runs(),
            Arc::new(HeartbeatClock),
            NonZeroUsize::MIN,
        ));
        let executor = GraphExecutor::new(
            database.graph_dispatch(),
            Arc::new(GraphResources::new(NonZeroUsize::MIN, HashMap::new())),
            Arc::new(GraphUtilityRunner::new(worker.clone())),
        );

        // Act
        let receipts = executor
            .run_ready(
                "graph",
                graph.revision,
                Path::new("workspace"),
                PermissionMode::AutoEdit,
            )
            .await?;
        worker.shutdown().await;

        // Assert
        assert_eq!(receipts.len(), 1);
        let result = database
            .graphs()
            .list_results("graph", "first", "response")
            .await?
            .remove(0);
        assert_eq!(result.result.reference.schema, profile);
        assert_eq!(result.result.state, GraphResultState::Validated);
        assert!(
            provider.requests.lock().expect("requests")[0]
                .prompt
                .contains("Inspect \"file.rs\"")
        );
    }

    Ok(())
}
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn dropped_driver_retains_env_until_inherited_cancellation_cleanup_settles()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let database = Database::open_in_memory().await?;
    populate(&database).await?;
    let mut graph = graph();
    graph.nodes.truncate(1);
    database.graphs().save_graph(&graph, None).await?;
    let provider = Arc::new(CleanupProvider::default());
    let worker = Arc::new(RunWorker::with_client(
        provider.clone(),
        database.runs(),
        Arc::new(HeartbeatClock),
        NonZeroUsize::MIN,
    ));
    let resources = Arc::new(GraphResources::new(
        NonZeroUsize::MIN,
        HashMap::from([("budget".into(), ProviderCallBudget::new(1))]),
    ));
    let environment = resources.environment(&graph.nodes[0].env_id);
    let executor = GraphExecutor::new(
        database.graph_dispatch(),
        resources,
        Arc::new(GraphUtilityRunner::new(worker.clone())),
    );
    let cancellation = CancellationToken::new();
    let scope = RunScope {
        cancellation: Some(cancellation.clone()),
        ..RunScope::default()
    };
    let driver = tokio::spawn(in_scope(scope, async move {
        executor
            .run_ready(
                "graph",
                NonZeroU64::MIN,
                Path::new("workspace"),
                PermissionMode::ReadOnly,
            )
            .await
    }));
    provider.entered.notified().await;

    // Act
    driver.abort();
    assert!(driver.await.expect_err("driver aborted").is_cancelled());
    cancellation.cancel();
    provider.canceled.notified().await;

    // Assert
    assert!(environment.try_write().is_err());
    provider.release.notify_one();
    worker.shutdown().await;
    let _settled = environment.write().await;
    let status: String = sqlx::query_scalar("SELECT status FROM agent_run")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(status, "canceled");
    assert_eq!(
        database
            .graphs()
            .list_executions("graph", graph.revision)
            .await?
            .len(),
        1
    );

    Ok(())
}

#[tokio::test]
async fn failed_branch_settles_peers_and_blocks_join_without_retrying() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let database = Database::open_in_memory().await?;
    populate(&database).await?;
    let graph = graph();
    database.graphs().save_graph(&graph, None).await?;
    let provider = Arc::new(Provider::failing_readers());
    let worker = Arc::new(RunWorker::with_client(
        provider.clone(),
        database.runs(),
        Arc::new(HeartbeatClock),
        NonZeroUsize::new(3).expect("capacity"),
    ));
    let executor = GraphExecutor::new(
        database.graph_dispatch(),
        Arc::new(GraphResources::new(
            NonZeroUsize::new(3).expect("capacity"),
            HashMap::from([("budget".into(), ProviderCallBudget::new(3))]),
        )),
        Arc::new(GraphUtilityRunner::new(worker.clone())),
    );

    // Act
    let result = executor
        .run_ready(
            "graph",
            graph.revision,
            Path::new("workspace"),
            PermissionMode::AutoEdit,
        )
        .await;
    let redelivery = executor
        .run_ready(
            "graph",
            graph.revision,
            Path::new("workspace"),
            PermissionMode::AutoEdit,
        )
        .await?;
    worker.shutdown().await;

    // Assert
    assert!(
        result
            .expect_err("branch failed")
            .to_string()
            .contains("scripted branch failure")
    );
    assert_eq!(redelivery, Vec::<ag_worker::GraphExecutionRecord>::new());
    assert_eq!(provider.requests.lock().expect("requests").len(), 2);
    let statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM agent_run ORDER BY rowid")
        .fetch_all(database.pool())
        .await?;
    assert_eq!(statuses, ["completed", "failed"]);
    assert_eq!(
        database
            .graphs()
            .list_results("graph", "first", "response")
            .await?
            .len(),
        1
    );
    assert_eq!(
        database
            .graphs()
            .list_results("graph", "join", "response")
            .await?,
        Vec::<ag_store::StoredGraphResult>::new()
    );

    Ok(())
}

#[tokio::test]
async fn parallel_branches_join_through_workers_and_keep_exact_inputs_without_replay()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let database = Database::open_in_memory().await?;
    populate(&database).await?;
    let graph = graph();
    database.graphs().save_graph(&graph, None).await?;
    let provider = Arc::new(Provider::parallel_readers());
    let worker = Arc::new(RunWorker::with_client(
        provider.clone(),
        database.runs(),
        Arc::new(HeartbeatClock),
        NonZeroUsize::new(3).expect("capacity"),
    ));
    let budget = ProviderCallBudget::new(3);
    let resources = Arc::new(GraphResources::new(
        NonZeroUsize::new(3).expect("capacity"),
        HashMap::from([("budget".into(), budget.clone())]),
    ));
    let executor = GraphExecutor::new(
        database.graph_dispatch(),
        resources,
        Arc::new(GraphUtilityRunner::new(worker.clone())),
    );

    // Act
    let receipts = executor
        .run_ready(
            &graph.id,
            graph.revision,
            Path::new("workspace"),
            PermissionMode::AutoEdit,
        )
        .await?;
    let redelivery = executor
        .run_ready(
            &graph.id,
            graph.revision,
            Path::new("workspace"),
            PermissionMode::AutoEdit,
        )
        .await?;
    worker.shutdown().await;

    // Assert
    assert_eq!(
        receipts
            .iter()
            .map(|record| record.node_id.as_str())
            .collect::<Vec<_>>(),
        ["first", "second", "join"]
    );
    assert_eq!(redelivery, Vec::<ag_worker::GraphExecutionRecord>::new());
    assert_eq!(budget.remaining(), 0);
    {
        let requests = provider.requests.lock().expect("requests");
        assert_eq!(requests.len(), 3);
        assert!(
            requests
                .iter()
                .all(|request| request.permission_mode == PermissionMode::ReadOnly)
        );
        assert!(requests[2].prompt.contains("Bound input first"));
        assert!(requests[2].prompt.contains("Bound input second"));
    }
    assert_eq!(receipts[2].inputs.len(), 2);
    for input in &receipts[2].inputs {
        let result = database
            .graphs()
            .load_result(&input.result.id)
            .await?
            .expect("input");
        assert_eq!(result.result.reference, input.result);
        assert_eq!(result.result.state, GraphResultState::Validated);
        assert!(result.acceptance.is_none());
    }
    let statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM agent_run ORDER BY rowid")
        .fetch_all(database.pool())
        .await?;
    assert_eq!(statuses, ["completed", "completed", "completed"]);

    Ok(())
}

#[tokio::test]
async fn competing_admission_has_one_winner_and_reopening_does_not_replay()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::Builder::new()
        .prefix("graph-dispatch-")
        .tempdir_in(".")?;
    let path = directory.path().join("graph.sqlite");
    let database = Database::open(&path).await?;
    populate(&database).await?;
    database.graphs().save_graph(&graph(), None).await?;
    let first = database.graph_dispatch();
    let second = database.graph_dispatch();

    // Act
    let (first, second) = tokio::join!(
        first.claim(
            "graph",
            NonZeroU64::MIN,
            "first",
            "delivery-1",
            Path::new("workspace")
        ),
        second.claim(
            "graph",
            NonZeroU64::MIN,
            "first",
            "delivery-2",
            Path::new("workspace")
        )
    );
    let admissions = [first?, second?];
    database.pool().close().await;
    drop(database);
    let reopened = Database::open(&path).await?;
    let redelivery = reopened
        .graph_dispatch()
        .claim(
            "graph",
            NonZeroU64::MIN,
            "first",
            "restart",
            Path::new("workspace"),
        )
        .await?;

    // Assert
    assert_eq!(
        admissions
            .iter()
            .filter(|admission| matches!(admission, GraphAdmission::Claimed(_)))
            .count(),
        1
    );
    assert_eq!(
        admissions
            .iter()
            .filter(|admission| matches!(admission, GraphAdmission::Existing(_)))
            .count(),
        1
    );
    assert!(matches!(redelivery, GraphAdmission::Existing(_)));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_run")
        .fetch_one(reopened.pool())
        .await?;
    assert_eq!(count, 1);
    reopened.pool().close().await;

    Ok(())
}

#[tokio::test]
async fn explicit_acceptance_releases_a_blocked_join_on_the_next_call() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let database = Database::open_in_memory().await?;
    populate(&database).await?;
    let mut graph = graph();
    graph.nodes[2].dependencies[0].gate = ag_session::GraphCompletionGate::ResultAccepted {
        output_name: "response".into(),
    };
    database.graphs().save_graph(&graph, None).await?;
    let provider = Arc::new(Provider::parallel_readers());
    let worker = Arc::new(RunWorker::with_client(
        provider,
        database.runs(),
        Arc::new(HeartbeatClock),
        NonZeroUsize::new(3).expect("capacity"),
    ));
    let executor = GraphExecutor::new(
        database.graph_dispatch(),
        Arc::new(GraphResources::new(
            NonZeroUsize::new(3).expect("capacity"),
            HashMap::from([("budget".into(), ProviderCallBudget::new(3))]),
        )),
        Arc::new(GraphUtilityRunner::new(worker.clone())),
    );
    let first = executor
        .run_ready(
            "graph",
            graph.revision,
            Path::new("workspace"),
            PermissionMode::ReadOnly,
        )
        .await?;
    assert_eq!(first.len(), 2);
    let result = database
        .graphs()
        .list_results("graph", "first", "response")
        .await?
        .remove(0);

    // Act
    database
        .graphs()
        .accept_result(
            &result.result.reference,
            &GraphEvidence {
                actor: "user".into(),
                reason: "Accept exact output".into(),
            },
        )
        .await?;
    let released = executor
        .run_ready(
            "graph",
            graph.revision,
            Path::new("workspace"),
            PermissionMode::ReadOnly,
        )
        .await?;
    worker.shutdown().await;

    // Assert
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].inputs[0].result, result.result.reference);

    Ok(())
}

#[tokio::test]
async fn unsupported_profiles_and_unknown_budgets_fail_before_admission()
-> Result<(), Box<dyn Error>> {
    // Arrange
    for unsupported in [false, true] {
        let database = Database::open_in_memory().await?;
        populate(&database).await?;
        let mut graph = graph();
        if unsupported {
            graph.nodes[1]
                .request
                .attachments
                .push(ag_protocol::TurnPromptAttachment {
                    local_image_path: "image.png".into(),
                    placeholder: "[Image #1]".into(),
                });
        }
        database.graphs().save_graph(&graph, None).await?;
        let worker = Arc::new(RunWorker::with_client(
            Arc::new(Provider::default()),
            database.runs(),
            Arc::new(HeartbeatClock),
            NonZeroUsize::MIN,
        ));
        let budgets = if unsupported {
            HashMap::from([("budget".into(), ProviderCallBudget::new(3))])
        } else {
            HashMap::new()
        };
        let executor = GraphExecutor::new(
            database.graph_dispatch(),
            Arc::new(GraphResources::new(NonZeroUsize::MIN, budgets)),
            Arc::new(GraphUtilityRunner::new(worker.clone())),
        );

        // Act
        let result = executor
            .run_ready(
                "graph",
                graph.revision,
                Path::new("workspace"),
                PermissionMode::AutoEdit,
            )
            .await;
        worker.shutdown().await;

        // Assert
        assert!(result.is_err());
        assert_eq!(
            database
                .graphs()
                .list_executions("graph", graph.revision)
                .await?,
            Vec::<ag_worker::GraphExecutionRecord>::new()
        );
    }

    Ok(())
}

#[tokio::test]
async fn external_env_lease_defers_dispatch_until_the_host_releases_it()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let database = Database::open_in_memory().await?;
    populate(&database).await?;
    let mut graph = graph();
    graph.nodes.truncate(1);
    graph.nodes[0].execution.workspace_access = GraphWorkspaceAccess::ExclusiveWrite;
    graph.nodes[0].definition.read_only = false;
    database.graphs().save_graph(&graph, None).await?;
    let provider = Arc::new(Provider::default());
    let worker = Arc::new(RunWorker::with_client(
        provider.clone(),
        database.runs(),
        Arc::new(HeartbeatClock),
        NonZeroUsize::MIN,
    ));
    let resources = Arc::new(GraphResources::new(
        NonZeroUsize::MIN,
        HashMap::from([("budget".into(), ProviderCallBudget::new(1))]),
    ));
    let environment = resources.environment(&graph.nodes[0].env_id);
    let lease = environment.read().await;
    let executor = GraphExecutor::new(
        database.graph_dispatch(),
        resources,
        Arc::new(GraphUtilityRunner::new(worker.clone())),
    );

    // Act
    assert_eq!(
        executor
            .run_ready(
                "graph",
                graph.revision,
                Path::new("workspace"),
                PermissionMode::AutoEdit
            )
            .await?,
        Vec::<ag_worker::GraphExecutionRecord>::new()
    );
    drop(lease);
    let released = executor
        .run_ready(
            "graph",
            graph.revision,
            Path::new("workspace"),
            PermissionMode::ReadOnly,
        )
        .await?;
    worker.shutdown().await;

    // Assert
    assert_eq!(released.len(), 1);
    assert_eq!(
        provider.requests.lock().expect("requests")[0].permission_mode,
        PermissionMode::ReadOnly
    );

    Ok(())
}
