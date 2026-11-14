use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{
    AgentRequestKind, MockAgentChannel, OneShotError, OneShotSubmission, SessionStats, TurnRequest,
    TurnResult,
};
use ag_git::MockGitClient;
use ag_protocol::{AgentResponse, TurnPrompt};
use ag_session::{
    ExecutionGraph, ExecutionGraphProgress, ExecutionGraphStatus, GraphContext, GraphNode,
    GraphNodeContinuation, GraphNodeStatus, GraphPostProcessing, GraphWorkspaceAccess,
    StartExecutionGraphRequest,
};
use ag_store::{DbError, MockGraphRepository, StoredGraphResult};
use ag_worker::{GraphExecutor, GraphNodeRunner, GraphRunRequest, MockRunClient, SessionRunClient};
use async_trait::async_trait;
use tempfile::TempDir;
use tokio::sync::mpsc;

use crate::app::App;
use crate::app::execution_graph::GraphDriverRegistration;
use crate::app::execution_graph::tests::{graph, seed_session};
use crate::app::graph_node_reply::tests::{app_with, messages, screenshot};
use crate::app::graph_proposal_reply::tests::{apply_next_command, wait_for_message};
use crate::app::prompt_intent::{PromptSessionMode, PromptSubmission, PromptWorkflowOutcome};
use crate::app::service::MockSessionRunFactory;
use crate::domain::session::SessionId;
use crate::infra::fs::MockFsClient;
use crate::presentation::execution_graph::tests::{acceptance_gated_view, consumed_gated_view};
use crate::test_support::{new_test_app_with_clients, test_app_clients};

#[tokio::test]
async fn start_implementing_in_chat_accepts_the_awaited_output_and_continues_the_graph() {
    // Arrange
    let (mut app, _directory, mut observed_rx) = reader_graph_app().await;
    let _consumer = app.sessions.foreground_consumer();
    let service = app.session_service();
    let request = StartExecutionGraphRequest {
        graph: reader_graph(),
        provider_call_limits: BTreeMap::from([("calls".into(), 2)]),
    };
    app.drive_session_request(service.start_execution_graph(request))
        .await
        .expect("start");
    app.drive_session_request(async { observed_rx.recv().await.expect("first turn") })
        .await;
    wait_for_node(&mut app, 0, GraphNodeStatus::Succeeded).await;

    // Act
    let outcome = app.submit_prompt(submission("Start implementing")).await;
    let second = tokio::time::timeout(
        Duration::from_secs(5),
        app.drive_session_request(async { observed_rx.recv().await.expect("second turn") }),
    )
    .await
    .expect("accepted output dispatches the waiting node");
    let completed = wait_for_node(&mut app, 1, GraphNodeStatus::Succeeded).await;
    let notice = wait_for_message(&app, "Accepted").await;

    // Assert
    assert_eq!(
        outcome,
        PromptWorkflowOutcome::ShowSession {
            session_id: "sess".into()
        }
    );
    assert!(second.prompt.agent_text().contains("Plan ready"));
    assert_eq!(completed.status, ExecutionGraphStatus::Completed);
    assert_eq!(notice, "Accepted `response` from `first` · continuing");
    assert!(
        messages(&app)
            .await
            .contains(&("user_prompt".into(), "Start implementing".into()))
    );
    app.services.wait_for_cleanup_tasks(None).await;
}

#[tokio::test]
async fn revise_reply_sends_guidance_to_the_producer() {
    // Arrange
    let (graph, progress) = gated();
    let expected_execution = progress.nodes[0].executions[0].clone();
    let mut repository = listing(graph, progress);
    repository
        .expect_continue_node()
        .once()
        .withf(move |request| {
            request.graph_id == "graph"
                && request.node_id == "first"
                && request.execution == expected_execution
                && request.kind
                    == GraphNodeContinuation::Reply {
                        attachments: Vec::new(),
                        message: "Also add tests".into(),
                    }
        })
        .returning(|_| Ok(true));
    let (mut app, _directory, _driver) = decided_app(repository, "revise").await;
    let _consumer = app.sessions.foreground_consumer();

    // Act
    let outcome = app
        .submit_graph_output_reply(
            &SessionId::from("sess"),
            &TurnPrompt::from_text("Also add tests".into()),
        )
        .await;
    apply_next_command(&mut app).await;
    let notice = wait_for_message(&app, "Revision guidance").await;

    // Assert
    assert_eq!(
        outcome,
        Some(PromptWorkflowOutcome::ShowSession {
            session_id: "sess".into()
        })
    );
    assert_eq!(notice, "Revision guidance sent to `first` · continuing");
    assert!(
        app.services
            .output_reply_decisions
            .lock()
            .expect("decision lock")
            .is_empty()
    );
}

#[tokio::test]
async fn revise_reply_reports_a_producer_that_cannot_take_guidance() {
    // Arrange
    let (mut graph, progress) = gated();
    graph.nodes[0].execution.context = GraphContext::Fresh;
    let (mut app, _directory, _driver) = decided_app(listing(graph, progress), "revise").await;

    // Act
    app.submit_graph_output_reply(
        &SessionId::from("sess"),
        &TurnPrompt::from_text("Different approach".into()),
    )
    .await;
    let notice = wait_for_message(&app, "Graph output:").await;

    // Assert
    assert!(
        notice.contains(
            "`first` cannot take revision guidance. Reply with clear acceptance, or accept or \
             cancel it from `w`."
        ),
        "{notice}"
    );
}

#[tokio::test]
async fn reply_with_attachments_to_a_producer_that_cannot_take_guidance_keeps_the_composer() {
    // Arrange
    let (mut graph, progress) = gated();
    graph.nodes[0].execution.context = GraphContext::Fresh;
    let (mut app, _directory) = app_with(listing(graph, progress)).await;
    let _driver = live_driver(&app);
    let mut client = MockRunClient::new();
    client.expect_submit().never();
    app.services.set_run_client(Arc::new(client));
    let prompt = TurnPrompt {
        attachments: vec![screenshot()],
        ..TurnPrompt::from_text("Go ahead like this [Image #1]".into())
    };

    // Act
    let outcome = app
        .submit_graph_output_reply(&SessionId::from("sess"), &prompt)
        .await;

    // Assert
    assert_eq!(outcome, Some(PromptWorkflowOutcome::KeepPrompt));
    let messages = messages(&app).await;
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0].1.contains(
            "`first` cannot take images as revision guidance. Remove them to accept its output in \
             chat, or use `w`."
        ),
        "{messages:?}"
    );
    assert!(
        app.services
            .output_reply_decisions
            .lock()
            .expect("decision lock")
            .is_empty()
    );
}

#[tokio::test]
async fn reply_with_attachments_to_an_output_a_started_dependent_consumed_keeps_the_composer() {
    // Arrange
    let view = consumed_gated_view();
    let (mut app, _directory) = app_with(listing(
        view.graphs[0].clone(),
        view.progress.expect("consumed progress"),
    ))
    .await;
    let _driver = live_driver(&app);
    let mut client = MockRunClient::new();
    client.expect_submit().never();
    app.services.set_run_client(Arc::new(client));
    let prompt = TurnPrompt {
        attachments: vec![screenshot()],
        ..TurnPrompt::from_text("Redo it like this [Image #1]".into())
    };

    // Act
    let outcome = app
        .submit_graph_output_reply(&SessionId::from("sess"), &prompt)
        .await;

    // Assert
    assert_eq!(outcome, Some(PromptWorkflowOutcome::KeepPrompt));
    let messages = messages(&app).await;
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0]
            .1
            .contains("`first` cannot take images as revision guidance."),
        "{messages:?}"
    );
}

#[tokio::test]
async fn reply_with_attachments_revises_without_deciding() {
    // Arrange
    let (graph, progress) = gated();
    let mut repository = listing(graph, progress);
    repository
        .expect_continue_node()
        .once()
        .withf(|request| {
            request.node_id == "first"
                && request.kind
                    == GraphNodeContinuation::Reply {
                        attachments: vec![screenshot()],
                        message: "Start implementing, match this [Image #1]".into(),
                    }
        })
        .returning(|_| Ok(true));
    let (mut app, _directory) = app_with(repository).await;
    let _driver = live_driver(&app);
    let mut client = MockRunClient::new();
    client.expect_submit().never();
    app.services.set_run_client(Arc::new(client));
    let _consumer = app.sessions.foreground_consumer();
    let prompt = TurnPrompt {
        attachments: vec![screenshot()],
        ..TurnPrompt::from_text("Start implementing, match this [Image #1]".into())
    };

    // Act
    let outcome = app
        .submit_graph_output_reply(&SessionId::from("sess"), &prompt)
        .await;
    apply_next_command(&mut app).await;
    let notice = wait_for_message(&app, "Revision guidance").await;

    // Assert
    assert_eq!(
        outcome,
        Some(PromptWorkflowOutcome::ShowSession {
            session_id: "sess".into()
        })
    );
    assert_eq!(notice, "Revision guidance sent to `first` · continuing");
}

#[tokio::test]
async fn accept_reply_reports_a_stale_output_instead_of_retargeting_it() {
    // Arrange
    let (graph, progress) = gated();
    let mut stale = graph.clone();
    stale.revision = stale.revision.saturating_add(1);
    let mut repository = listing(graph, progress);
    repository
        .expect_load_graph()
        .returning(move |_, _| Ok(Some(stale.clone())));
    let (mut app, _directory, _driver) = decided_app(repository, "accept").await;
    let _consumer = app.sessions.foreground_consumer();

    // Act
    app.submit_graph_output_reply(
        &SessionId::from("sess"),
        &TurnPrompt::from_text("Go ahead".into()),
    )
    .await;
    apply_next_command(&mut app).await;
    let notice = wait_for_message(&app, "Graph output:").await;

    // Assert
    assert!(notice.contains("Stale graph acceptance"), "{notice}");
}

#[tokio::test]
async fn reply_while_the_previous_one_is_decided_keeps_the_composer() {
    // Arrange
    let (graph, progress) = gated();
    let (mut app, _directory, _driver) = decided_app(listing(graph, progress), "accept").await;
    app.services
        .output_reply_decisions
        .lock()
        .expect("decision lock")
        .insert("result".into());

    // Act
    let outcome = app
        .submit_graph_output_reply(
            &SessionId::from("sess"),
            &TurnPrompt::from_text("Go ahead".into()),
        )
        .await;

    // Assert
    assert_eq!(outcome, Some(PromptWorkflowOutcome::KeepPrompt));
    let messages = messages(&app).await;
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0]
            .1
            .contains("Still deciding on your previous reply to `first`"),
        "{messages:?}"
    );
}

#[tokio::test]
async fn reply_for_an_unloaded_session_keeps_the_composer_without_deciding() {
    // Arrange
    let (graph, progress) = gated();
    let (mut app, _directory, _driver) = decided_app(listing(graph, progress), "accept").await;

    // Act
    let outcome = app
        .submit_graph_output_reply(
            &SessionId::from("ghost"),
            &TurnPrompt::from_text("Go ahead".into()),
        )
        .await;

    // Assert
    assert_eq!(outcome, Some(PromptWorkflowOutcome::KeepPrompt));
    assert!(
        app.services
            .output_reply_decisions
            .lock()
            .expect("decision lock")
            .is_empty()
    );
}

#[tokio::test]
async fn graphs_without_an_awaited_output_leave_ordinary_routing() {
    for case in [
        "paused",
        "no-progress",
        "cancelled",
        "accepted",
        "no-execution",
    ] {
        // Arrange
        let (graph, mut progress) = gated();
        match case {
            "cancelled" => progress.status = ExecutionGraphStatus::Cancelled,
            "accepted" => progress.nodes[1].blocked.clear(),
            "no-execution" => progress.nodes[0].executions.clear(),
            _ => {}
        }
        let mut repository = MockGraphRepository::new();
        repository
            .expect_list_graphs()
            .returning(move |_| Ok(vec![graph.clone()]));
        repository
            .expect_progress()
            .returning(move |_| Ok((case != "no-progress").then(|| progress.clone())));
        let (mut app, _directory) = app_with(repository).await;
        let _driver = (case != "paused").then(|| live_driver(&app));

        // Act
        let outcome = app
            .submit_graph_output_reply(
                &SessionId::from("sess"),
                &TurnPrompt::from_text("Go ahead".into()),
            )
            .await;

        // Assert
        assert_eq!(outcome, None, "{case}");
        assert!(messages(&app).await.is_empty(), "{case}");
    }
}

#[tokio::test]
async fn failed_reads_report_the_error_and_keep_the_composer() {
    for fault in ["list", "progress", "result"] {
        // Arrange
        let (graph, progress) = gated();
        let mut repository = MockGraphRepository::new();
        repository.expect_list_graphs().returning(move |_| {
            if fault == "list" {
                Err(unavailable())
            } else {
                Ok(vec![graph.clone()])
            }
        });
        repository.expect_progress().returning(move |_| {
            if fault == "progress" {
                Err(unavailable())
            } else {
                Ok(Some(progress.clone()))
            }
        });
        repository
            .expect_load_result()
            .returning(|_| Err(unavailable()));
        let (mut app, _directory) = app_with(repository).await;
        let _driver = live_driver(&app);

        // Act
        let outcome = app
            .submit_graph_output_reply(
                &SessionId::from("sess"),
                &TurnPrompt::from_text("Go ahead".into()),
            )
            .await;

        // Assert
        assert_eq!(outcome, Some(PromptWorkflowOutcome::KeepPrompt), "{fault}");
        let messages = messages(&app).await;
        assert_eq!(messages.len(), 1, "{fault}: {messages:?}");
        assert!(
            messages[0].1.contains("Graph output:") && messages[0].1.contains("Read failed"),
            "{fault}: {messages:?}"
        );
    }
}

/// Blocked graph whose `second` node waits for acceptance of `first`'s
/// validated `response` output.
fn gated() -> (ExecutionGraph, ExecutionGraphProgress) {
    let view = acceptance_gated_view();

    (
        view.graphs[0].clone(),
        view.progress.expect("gated progress"),
    )
}

/// Repository that lists the gated graph and its stored output body.
fn listing(graph: ExecutionGraph, progress: ExecutionGraphProgress) -> MockGraphRepository {
    let result = progress.nodes[0].results[0].clone();
    let mut repository = MockGraphRepository::new();
    repository
        .expect_list_graphs()
        .returning(move |_| Ok(vec![graph.clone()]));
    repository
        .expect_progress()
        .returning(move |_| Ok(Some(progress.clone())));
    repository.expect_load_result().returning(move |_| {
        Ok(Some(StoredGraphResult {
            acceptance: None,
            body: r#"{"answer":"Plan body"}"#.into(),
            result: result.clone(),
            validation: None,
        }))
    });

    repository
}

/// App over `repository` with a live gated-graph driver whose reply decision
/// answers `decision` after checking that the prompt carries the decoded
/// output.
async fn decided_app(
    repository: MockGraphRepository,
    decision: &'static str,
) -> (App, TempDir, GraphDriverRegistration) {
    let (mut app, directory) = app_with(repository).await;
    let driver = live_driver(&app);
    let mut client = MockRunClient::new();
    client.expect_submit().returning(move |request| {
        assert_eq!(request.request_kind, AgentRequestKind::UtilityPrompt);
        assert!(request.prompt.contains(
            r#""Node `first` (Inspect first) produced output `response`:\n\nPlan body""#
        ));

        Ok(OneShotSubmission {
            response: AgentResponse::plain(decision),
            stats: SessionStats::default(),
        })
    });
    app.services.set_run_client(Arc::new(client));

    (app, directory, driver)
}

/// Registers a live driver for the gated graph, so chat replies may act on
/// it; dropping the registration pauses the graph again.
fn live_driver(app: &App) -> GraphDriverRegistration {
    let executor = GraphExecutor::new(
        app.services.db().graph_dispatch(),
        Arc::clone(&app.services.graph_resources),
        Arc::new(IdleRunner),
    );

    GraphDriverRegistration::register(&app.services.graph_drivers, &gated().0, Arc::new(executor))
}

/// Two-reader graph whose second node waits for acceptance of the first
/// node's output.
fn reader_graph() -> ExecutionGraph {
    let mut graph = graph();
    let second = &mut graph.nodes[1];
    second.definition.read_only = true;
    second.execution.post_processing = GraphPostProcessing::None;
    second.execution.workspace_access = GraphWorkspaceAccess::SharedRead;

    graph
}

/// App whose graph tasks answer `Plan ready` and whose reply decision
/// accepts.
async fn reader_graph_app() -> (App, TempDir, mpsc::UnboundedReceiver<TurnRequest>) {
    let (observed_tx, observed_rx) = mpsc::unbounded_channel();
    let mut factory = MockSessionRunFactory::new();
    factory.expect_create().returning(|id, _| {
        let mut channel = MockAgentChannel::new();
        channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));
        SessionRunClient::from_channel(id.to_string(), Arc::new(channel))
    });
    factory
        .expect_create_graph_task()
        .times(2)
        .returning(move |context, _, _| {
            let mut channel = MockAgentChannel::new();
            let observed = observed_tx.clone();
            channel
                .expect_run_turn()
                .once()
                .returning(move |_, request, _| {
                    request
                        .provider_call_budget
                        .as_ref()
                        .expect("budget")
                        .consume()
                        .expect("credit");
                    observed.send(request).expect("observe");
                    Box::pin(async {
                        Ok(TurnResult {
                            assistant_message: AgentResponse::plain("Plan ready"),
                            context_reset: false,
                            harness_report: None,
                            input_tokens: 1,
                            output_tokens: 1,
                            provider_conversation_id: None,
                        })
                    })
                });
            channel
                .expect_shutdown_session()
                .once()
                .returning(|_| Box::pin(async { Ok(()) }));
            Ok(SessionRunClient::from_channel(
                context.into(),
                Arc::new(channel),
            ))
        });
    let (mut app, directory) =
        new_test_app_with_clients(test_app_clients().with_session_run_factory(Arc::new(factory)))
            .await;
    let mut fs = MockFsClient::new();
    fs.expect_is_dir().returning(|_| true);
    app.services.set_fs_client(Arc::new(fs));
    let mut git = MockGitClient::new();
    git.expect_detect_git_info()
        .returning(|_| Box::pin(async { Some("wt/sess".into()) }));
    git.expect_main_checkout_working_tree()
        .returning(|_| Box::pin(async { Ok(None) }));
    git.expect_diff()
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    app.services.set_git_client(Arc::new(git));
    seed_session(&mut app.sessions, app.services.db()).await;
    let mut client = MockRunClient::new();
    client.expect_submit().once().returning(|request| {
        assert!(request.prompt.contains(r#""Start implementing""#));

        Ok(OneShotSubmission {
            response: AgentResponse::plain("accept"),
            stats: SessionStats::default(),
        })
    });
    app.services.set_run_client(Arc::new(client));

    (app, directory, observed_rx)
}

/// Drives foreground commands until node `index` reaches `status`.
async fn wait_for_node(
    app: &mut App,
    index: usize,
    status: GraphNodeStatus,
) -> ExecutionGraphProgress {
    let db = app.services.db().clone();
    let mut wake = app.services.graph_wake.subscribe();

    tokio::time::timeout(
        Duration::from_secs(5),
        app.drive_session_request(async move {
            loop {
                wake.borrow_and_update();
                let progress = db
                    .graphs()
                    .progress("graph")
                    .await
                    .expect("progress")
                    .expect("graph");
                if progress.nodes[index].status == status {
                    return progress;
                }
                wake.changed().await.expect("graph wake");
            }
        }),
    )
    .await
    .expect("node status")
}

fn submission(text: &str) -> PromptSubmission {
    PromptSubmission {
        prompt: TurnPrompt::from_text(text.into()),
        session_id: "sess".into(),
        session_mode: PromptSessionMode::Existing,
    }
}

fn unavailable() -> DbError {
    DbError::InvalidData {
        entity: "graph",
        reason: "Read failed".into(),
    }
}

/// Runner of a registered driver that never dispatches in these tests.
struct IdleRunner;

#[async_trait]
impl GraphNodeRunner for IdleRunner {
    fn supports(&self, _node: &GraphNode) -> bool {
        true
    }

    async fn run(&self, _request: GraphRunRequest) -> Result<String, OneShotError> {
        Err(OneShotError::new("idle runner never executes"))
    }
}
