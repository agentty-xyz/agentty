use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{AgentRequestKind, MockAgentChannel, TurnResult};
use ag_git::MockGitClient;
use ag_protocol::{AgentResponse, QuestionItem};
use ag_session::{
    ContinueGraphNodeRequest, ExecutionGraphProgress, ExecutionGraphStatus, GraphBlockedReason,
    GraphControlRequest, GraphNodeContinuation, GraphNodeStatus, GraphQuestionAnswer,
    ResumeExecutionGraphRequest, StartExecutionGraphRequest,
};
use ag_worker::SessionRunClient;
use tokio::sync::{mpsc, watch};

use super::{graph, seed_session};
use crate::app::App;
use crate::app::service::MockSessionRunFactory;
use crate::infra::db::AppRepositories;
use crate::infra::fs::MockFsClient;
use crate::test_support::{new_test_app_with_clients, test_app_clients};

async fn clarification_app() -> (
    App,
    tempfile::TempDir,
    mpsc::UnboundedReceiver<ag_contracts::TurnRequest>,
) {
    let (observed_tx, observed_rx) = mpsc::unbounded_channel();
    let mut factory = MockSessionRunFactory::new();
    factory.expect_create().once().returning(|id, _| {
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

    (app, directory, observed_rx)
}

#[tokio::test]
async fn graph_lifecycle_api_routes_clarification_into_an_isolated_new_attempt() {
    // Arrange
    let (mut app, _directory, mut observed_rx) = clarification_app().await;
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

    // Assert
    assert_eq!(completed.status, ExecutionGraphStatus::Completed);
    assert_eq!(queried.nodes[0].executions.len(), 2);
    let transcript = second
        .continuation
        .replay_transcript()
        .expect("isolated history");
    assert!(transcript.contains("Need target"));
    assert!(transcript.contains("Which target?"));
    assert!(transcript.contains("Local only"));
    assert_eq!(second.prompt.attachments, first.prompt.attachments);
    assert_eq!(
        app.services
            .db()
            .sessions()
            .load_session_messages("sess")
            .await
            .expect("ordinary history")
            .len(),
        0
    );
    assert!(observed_rx.try_recv().is_err());
    app.services.wait_for_cleanup_tasks(None).await;
}

#[tokio::test]
async fn graph_lifecycle_resume_is_explicit_and_cancellation_is_terminal() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let _consumer = app.sessions.foreground_consumer();
    let mut snapshot = graph();
    snapshot.nodes.truncate(1);
    app.services
        .db()
        .graphs()
        .save_graph(&snapshot, None)
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
        provider_call_limits: BTreeMap::from([("calls".into(), 0)]),
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
    let mut refilled = resume.clone();
    refilled.provider_call_limits.insert("calls".into(), 2);
    assert!(
        app.drive_session_request(service.resume_execution_graph(refilled))
            .await
            .is_err()
    );
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
