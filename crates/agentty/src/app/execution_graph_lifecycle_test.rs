//! Execution graph clarification, node question, resume, and cancellation unit
//! tests.

use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{AgentRequestKind, ContextCompaction, MockAgentChannel, TurnResult};
use ag_git::{DiffStats, MockGitClient};
use ag_protocol::{AgentResponse, QuestionItem, TurnPrompt};
use ag_session::{
    ContinueGraphNodeRequest, ExecutionGraph, ExecutionGraphProgress, ExecutionGraphStatus,
    ExtendGraphBudgetRequest, GraphBlockedReason, GraphControlRequest, GraphNodeContinuation,
    GraphNodeStatus, GraphQuestionAnswer, ResumeExecutionGraphRequest, StartExecutionGraphRequest,
};
use ag_worker::SessionRunClient;
use tokio::sync::{mpsc, watch};

use super::{graph, seed_session};
use crate::app::App;
use crate::app::prompt_intent::{PromptSessionMode, PromptSubmission, PromptWorkflowOutcome};
use crate::app::service::MockSessionRunFactory;
use crate::app::service::test_support::accepting_session_commands;
use crate::infra::db::AppRepositories;
use crate::infra::fs::MockFsClient;
use crate::test_support::{new_test_app_with_clients, test_app_clients};

async fn clarification_app(
    graph_tasks: usize,
) -> (
    App,
    tempfile::TempDir,
    mpsc::UnboundedReceiver<ag_contracts::TurnRequest>,
) {
    let (observed_tx, observed_rx) = mpsc::unbounded_channel();
    let mut factory = MockSessionRunFactory::new();
    factory
        .expect_session_commands()
        .returning(accepting_session_commands);
    factory.expect_create().once().returning(|id, _| {
        let mut channel = MockAgentChannel::new();
        channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));
        SessionRunClient::from_channel(id.to_string(), Arc::new(channel))
    });
    factory
        .expect_create_graph_task()
        .times(graph_tasks)
        .returning(move |context, _, _| {
            let mut channel = MockAgentChannel::new();
            let observed = observed_tx.clone();
            channel
                .expect_run_turn()
                .once()
                .returning(move |_, mut request, _| {
                    // Observe the request without its budget: a retained
                    // clone keeps the graph's spending writer open, so
                    // cleanup would wait out its shutdown deadline.
                    request
                        .provider_call_budget
                        .take()
                        .expect("budget")
                        .consume()
                        .expect("credit");
                    let resumed = request.continuation.replay_transcript().is_some();
                    observed.send(request).expect("observe");
                    Box::pin(async move {
                        let mut response =
                            AgentResponse::plain(if resumed { "Finished" } else { "Need target" });
                        if !resumed {
                            response.questions.push(QuestionItem::new("Which target?"));
                        }
                        Ok(TurnResult {
                            assistant_message: response,
                            context_reset: false,
                            harness_report: None,
                            provider_conversation_id: None,
                            usage: ag_contracts::TurnUsage::default(),
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
    git.expect_diff_stats()
        .returning(|_, _| Box::pin(async { Ok(DiffStats::default()) }));
    app.services.set_git_client(Arc::new(git));
    seed_session(&mut app.sessions, app.services.db()).await;

    (app, directory, observed_rx)
}

#[tokio::test]
async fn graph_lifecycle_api_routes_clarification_into_an_isolated_new_attempt() {
    // Arrange
    let (mut app, _directory, mut observed_rx) = clarification_app(2).await;
    let _consumer = app.sessions.foreground_consumer();
    let service = app.session_service();
    let mut snapshot = graph();
    snapshot.nodes.truncate(1);
    let request = StartExecutionGraphRequest {
        graph: snapshot,
        provider_call_limits: BTreeMap::from([("calls".into(), 2)]),
    };

    // Act
    app.drive_session_request(service.start_execution_graph(request))
        .await
        .expect("start");
    let first = tokio::time::timeout(
        Duration::from_secs(5),
        app.drive_session_request(async { observed_rx.recv().await.expect("initial turn") }),
    )
    .await
    .expect("initial provider handoff");
    assert_eq!(first.request_kind, AgentRequestKind::SessionStart);
    // Graph nodes never compact, so no hidden notice drops their history.
    assert_eq!(first.context_compaction, ContextCompaction::Off);
    assert_eq!(
        app.services
            .db()
            .graphs()
            .budget_limits("graph")
            .await
            .expect("limits"),
        BTreeMap::from([("calls".to_string(), 2)])
    );
    assert!(first.continuation.replay_transcript().is_none());
    let question = wait_for_status(&mut app, GraphNodeStatus::AwaitingAnswers).await;
    let action = ContinueGraphNodeRequest {
        action_id: uuid::Uuid::new_v4().to_string(),
        execution: question.nodes[0].executions[0].clone(),
        graph_id: "graph".into(),
        kind: GraphNodeContinuation::Answer {
            answers: vec![GraphQuestionAnswer {
                question: "Which target?".into(),
                answer: "Local only".into(),
            }],
        },
        node_id: "first".into(),
        revision: NonZeroU64::MIN,
    };
    app.drive_session_request(service.continue_execution_graph_node(action.clone()))
        .await
        .expect("answer");
    let second = tokio::time::timeout(
        Duration::from_secs(5),
        app.drive_session_request(async { observed_rx.recv().await.expect("continued turn") }),
    )
    .await
    .expect("continued provider handoff");
    assert_eq!(second.request_kind, AgentRequestKind::SessionStart);
    assert!(second.continuation.provider_conversation_id().is_none());
    let completed = wait_for_status(&mut app, GraphNodeStatus::Succeeded).await;
    app.drive_session_request(service.continue_execution_graph_node(action))
        .await
        .expect("duplicate answer");
    let queried = app
        .drive_session_request(service.get_execution_graph("graph"))
        .await
        .expect("query")
        .expect("graph");
    let late_extension = app
        .drive_session_request(service.extend_execution_graph_budget(budget_extension("late")))
        .await;

    // Assert
    assert_eq!(completed.status, ExecutionGraphStatus::Completed);
    assert_eq!(queried.nodes[0].executions.len(), 2);
    assert!(
        late_extension
            .expect_err("completed graph")
            .to_string()
            .contains("Completed graphs need no more provider calls")
    );
    let transcript = second
        .continuation
        .replay_transcript()
        .expect("isolated history");
    assert!(transcript.contains("Need target"));
    assert!(transcript.contains("Which target?"));
    assert!(!transcript.contains("Local only"));
    assert!(second.prompt.text.contains("Local only"));
    assert_eq!(second.prompt.attachments, first.prompt.attachments);
    assert_eq!(chat_messages(&app).await, [question_notice()]);
    assert!(observed_rx.try_recv().is_err());
    app.services.wait_for_cleanup_tasks(None).await;
}

#[tokio::test]
async fn graph_node_questions_are_asked_and_answered_in_session_chat() {
    // Arrange
    let (mut app, _directory, mut observed_rx) = clarification_app(2).await;
    let _consumer = app.sessions.foreground_consumer();
    let service = app.session_service();
    let mut snapshot = graph();
    snapshot.nodes.truncate(1);
    let request = StartExecutionGraphRequest {
        graph: snapshot,
        provider_call_limits: BTreeMap::from([("calls".into(), 2)]),
    };
    app.drive_session_request(service.start_execution_graph(request))
        .await
        .expect("start");
    wait_for_status(&mut app, GraphNodeStatus::AwaitingAnswers).await;
    let asked = chat_messages(&app).await;

    // Act
    let outcome = app
        .submit_prompt(PromptSubmission {
            prompt: TurnPrompt::from_text("Local only".into()),
            session_id: "sess".into(),
            session_mode: PromptSessionMode::Existing,
        })
        .await;
    let continued = tokio::time::timeout(
        Duration::from_secs(5),
        app.drive_session_request(async {
            observed_rx.recv().await.expect("initial turn");
            observed_rx.recv().await.expect("answered turn")
        }),
    )
    .await
    .expect("answered provider handoff");
    let completed = wait_for_status(&mut app, GraphNodeStatus::Succeeded).await;

    // Assert
    assert_eq!(asked, [question_notice()]);
    assert_eq!(
        outcome,
        PromptWorkflowOutcome::ShowSession {
            session_id: "sess".into()
        }
    );
    assert!(continued.prompt.text.contains("Local only"));
    assert_eq!(completed.status, ExecutionGraphStatus::Completed);
    assert_eq!(
        chat_messages(&app).await,
        [
            question_notice(),
            ("user_prompt".into(), "Local only".into()),
            (
                "workflow_notice".into(),
                "Answer sent to `first` · continuing".into()
            ),
        ]
    );
    app.services.wait_for_cleanup_tasks(None).await;
}

/// The graph node's clarification as asked in session chat.
fn question_notice() -> (String, String) {
    (
        "workflow_notice".into(),
        "Need target\n\n`first` needs your input:\n1. Which target?".into(),
    )
}

async fn chat_messages(app: &App) -> Vec<(String, String)> {
    app.services
        .db()
        .sessions()
        .load_session_messages("sess")
        .await
        .expect("session chat")
        .into_iter()
        .map(|message| (message.kind, message.content))
        .collect()
}

#[tokio::test]
async fn graph_lifecycle_resume_is_explicit_and_cancellation_is_terminal() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let _consumer = app.sessions.foreground_consumer();
    let mut snapshot = graph();
    snapshot.nodes.truncate(1);
    // A restored graph whose recorded spending exhausted its fixed limit.
    app.services
        .db()
        .graphs()
        .launch_graph(&snapshot, &BTreeMap::from([("calls".into(), 0)]))
        .await
        .expect("durable unstarted graph");
    let control = GraphControlRequest {
        graph_id: "graph".into(),
        revision: snapshot.revision,
    };
    let service = app.session_service();

    // Act / Assert: reading a restored graph never creates a driver.
    assert!(
        app.drive_session_request(service.get_execution_graph("missing"))
            .await
            .expect("missing query")
            .is_none()
    );
    let paused = app
        .drive_session_request(service.get_execution_graph("graph"))
        .await
        .expect("progress")
        .expect("graph");
    assert_eq!(paused.status, ExecutionGraphStatus::Paused);
    assert!(
        paused.nodes[0]
            .blocked
            .contains(&GraphBlockedReason::ResumeRequired)
    );
    assert!(
        app.services
            .graph_drivers
            .lock()
            .expect("drivers")
            .is_empty()
    );
    let resume = ResumeExecutionGraphRequest {
        graph: control.clone(),
    };
    app.drive_session_request(service.resume_execution_graph(resume.clone()))
        .await
        .expect("resume with exhausted budget");
    app.drive_session_request(service.resume_execution_graph(resume.clone()))
        .await
        .expect("same limits");
    let blocked = app
        .drive_session_request(service.get_execution_graph("graph"))
        .await
        .expect("blocked")
        .expect("graph");
    assert_eq!(blocked.status, ExecutionGraphStatus::Blocked);
    assert!(
        blocked.nodes[0]
            .blocked
            .contains(&GraphBlockedReason::BudgetExhausted("calls".into()))
    );
    assert_graph_without_ledger_cannot_resume(&mut app, &snapshot).await;
    let mut stale = resume.clone();
    stale.graph.revision = NonZeroU64::new(2).expect("revision");
    assert!(
        app.drive_session_request(service.resume_execution_graph(stale))
            .await
            .is_err()
    );
    app.drive_session_request(service.cancel_execution_graph(control.clone()))
        .await
        .expect("cancel");
    app.drive_session_request(service.cancel_execution_graph(control))
        .await
        .expect("duplicate cancel");
    assert!(
        app.drive_session_request(service.resume_execution_graph(resume))
            .await
            .is_err()
    );
    let cancelled = app
        .drive_session_request(service.get_execution_graph("graph"))
        .await
        .expect("cancelled")
        .expect("graph");
    assert_eq!(cancelled.status, ExecutionGraphStatus::Cancelled);
    assert_eq!(cancelled.nodes[0].executions.len(), 0);
    app.services.wait_for_cleanup_tasks(None).await;
}

#[tokio::test]
async fn budget_extension_is_recorded_once_and_releases_the_waiting_node() {
    // Arrange
    let (mut app, _directory, mut observed_rx) = clarification_app(1).await;
    let _consumer = app.sessions.foreground_consumer();
    let service = app.session_service();
    let mut snapshot = graph();
    snapshot.nodes.truncate(1);
    // A restored graph whose recorded spending exhausted its fixed limit.
    app.services
        .db()
        .graphs()
        .launch_graph(&snapshot, &BTreeMap::from([("calls".into(), 0)]))
        .await
        .expect("durable unstarted graph");
    app.drive_session_request(service.resume_execution_graph(ResumeExecutionGraphRequest {
        graph: GraphControlRequest {
            graph_id: "graph".into(),
            revision: snapshot.revision,
        },
    }))
    .await
    .expect("resume with exhausted budget");
    let extension = budget_extension("extension");
    let mut missing = budget_extension("missing");
    missing.graph.graph_id = "missing".into();

    // Act
    app.drive_session_request(service.extend_execution_graph_budget(extension.clone()))
        .await
        .expect("extend");
    app.drive_session_request(service.extend_execution_graph_budget(extension))
        .await
        .expect("redelivered extension");
    let turn = tokio::time::timeout(
        Duration::from_secs(5),
        app.drive_session_request(async { observed_rx.recv().await.expect("extended turn") }),
    )
    .await
    .expect("provider handoff after the extension");
    let question = wait_for_status(&mut app, GraphNodeStatus::AwaitingAnswers).await;
    let missing = app
        .drive_session_request(service.extend_execution_graph_budget(missing))
        .await;

    // Assert
    assert_eq!(turn.request_kind, AgentRequestKind::SessionStart);
    assert_eq!(question.nodes[0].executions.len(), 1);
    let remaining = app
        .services
        .graph_drivers
        .lock()
        .expect("drivers")
        .get("graph")
        .and_then(|driver| driver.executor.remaining_provider_calls("calls"));
    assert_eq!(remaining, Some(0));
    assert_eq!(
        app.services
            .db()
            .graphs()
            .budget_limits("graph")
            .await
            .expect("launch limits"),
        BTreeMap::from([("calls".to_string(), 0)])
    );
    assert!(
        missing
            .expect_err("missing graph")
            .to_string()
            .contains("Graph is missing")
    );
    app.services.stop_graph_drivers(None);
    app.services.wait_for_cleanup_tasks(None).await;
}

/// Adds one provider call to the `calls` budget of revision one of `graph`.
fn budget_extension(action_id: &str) -> ExtendGraphBudgetRequest {
    ExtendGraphBudgetRequest {
        action_id: action_id.into(),
        additional_calls: 1,
        budget_id: "calls".into(),
        graph: GraphControlRequest {
            graph_id: "graph".into(),
            revision: NonZeroU64::MIN,
        },
    }
}

async fn wait_for_status(app: &mut App, status: GraphNodeStatus) -> ExecutionGraphProgress {
    let db = app.services.db().clone();
    let wake = app.services.graph_wake.subscribe();

    tokio::time::timeout(
        Duration::from_secs(5),
        app.drive_session_request(wait_progress(db, wake, status)),
    )
    .await
    .expect("node settlement and graph wakeup")
}

async fn wait_progress(
    db: AppRepositories,
    mut wake: watch::Receiver<u64>,
    status: GraphNodeStatus,
) -> ExecutionGraphProgress {
    loop {
        wake.borrow_and_update();
        let progress = db
            .graphs()
            .progress("graph")
            .await
            .expect("progress")
            .expect("graph");
        if progress.nodes[0].status == status {
            return progress;
        }
        wake.changed().await.expect("settlement wake");
    }
}

/// A graph saved before durable budgets has no recorded limits to resume.
async fn assert_graph_without_ledger_cannot_resume(app: &mut App, snapshot: &ExecutionGraph) {
    let mut legacy = snapshot.clone();
    legacy.id = "legacy".into();
    app.services
        .db()
        .graphs()
        .save_graph(&legacy, None)
        .await
        .expect("graph saved before durable budgets");
    let service = app.session_service();
    let resumed = app
        .drive_session_request(service.resume_execution_graph(ResumeExecutionGraphRequest {
            graph: GraphControlRequest {
                graph_id: "legacy".into(),
                revision: snapshot.revision,
            },
        }))
        .await;

    assert!(
        resumed
            .expect_err("no recorded limits")
            .to_string()
            .contains("before provider-call spending was recorded")
    );
}
