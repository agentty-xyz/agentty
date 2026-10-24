use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use ag_contracts::{AgentError, MockAgentChannel, PermissionMode, ProviderCallBudget};
use ag_git::{GitError, MockGitClient};
use ag_protocol::QuestionItem;
use ag_session::{GraphPostProcessing, GraphWorkspaceAccess, TaskAttemptStatus};
use ag_worker::{GraphRunRequest, MockRunClient, ScheduledWork, SessionRunClient, WorkerHost};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::super::{
    ScheduledSessionCommand, SessionCommand, SessionWorkerContext, SessionWorkerHost,
    SessionWorkerService,
};
use super::support::{
    mock_fs_client_with_existing_directories, queue_helper_context, successful_turn_result,
    test_graph_resources,
};
use crate::app::execution_graph::tests::{claimed_dispatch, graph, run_request, task_operation_id};
use crate::app::service::MockSessionRunFactory;
use crate::app::session::workflow::graph_turn::{GraphTaskCommand, GraphTaskCompletion};
use crate::domain::session::Status;

#[tokio::test]
async fn selected_graph_writers_wait_for_background_reviews_before_running() {
    // Arrange
    let context = graph_context("readonly").await;
    let (task, receiver) = graph_task_fixture(&context, "readonly").await;
    let resources = test_graph_resources();
    let background = resources.background_readers(&task.request.dispatch.node.env_id);
    let review = background.clone().read_owned().await;
    let environment = resources.environment(&task.request.dispatch.node.env_id);
    let host = SessionWorkerHost {
        context,
        graph_resources: resources,
        graph_wake: tokio::sync::watch::channel(0).0,
        message_traces: Arc::default(),
        run_client: Arc::new(MockRunClient::new()),
    };
    let work = host.execute(ScheduledWork::Command(ScheduledSessionCommand::immediate(
        SessionCommand::RunGraphTask(Box::new(task)),
    )));
    tokio::pin!(work);

    // Act
    let waiting = tokio::select! {
        biased;
        () = &mut work => false,
        () = std::future::ready(()) => true,
    };

    // Assert: selecting a graph writer waits without blocking ordinary sync.
    assert!(waiting);
    assert!(environment.clone().try_write_owned().is_ok());
    drop(review);
    work.await;
    receiver
        .await
        .expect("completion")
        .expect("successful task");
    assert!(environment.try_write_owned().is_ok());
    assert!(background.try_write_owned().is_ok());
}

#[tokio::test]
async fn graph_task_failures_questions_and_cancellation_settle_without_chat_leaks() {
    for scenario in [
        "factory",
        "provider",
        "cleanup",
        "pre-canceled",
        "cleanup-canceled",
        "questions",
        "effects",
        "readonly",
        "stateless",
        "isolation",
        "utility",
        "poisoned-cancel",
        "unbudgeted-effects",
    ] {
        // Arrange
        let context = graph_context(scenario).await;
        let (task, receiver) = graph_task_fixture(&context, scenario).await;
        let execution = task.request.dispatch.record.execution.clone();
        if scenario == "poisoned-cancel" {
            let token = context.cancel_token.clone();
            std::thread::spawn(move || {
                let _guard = token.lock().expect("cancel token");
                std::panic::resume_unwind(Box::new("poison cancellation lock"));
            })
            .join()
            .expect_err("poisoned lock");
        }
        // Act
        let mut run_client = MockRunClient::new();
        if ["effects", "unbudgeted-effects"].contains(&scenario) {
            run_client
                .expect_submit()
                .returning(|_| Err(ag_contracts::OneShotError::new("repair failed")));
        }
        let result = SessionWorkerService::process_session_command(
            &context,
            &(Arc::new(run_client) as Arc<dyn ag_worker::RunClient>),
            SessionCommand::RunGraphTask(Box::new(task)),
        )
        .await
        .expect("selected");
        let response = receiver.await.expect("worker completion");
        let history = context
            .db
            .graphs()
            .load_task_turn(&execution)
            .await
            .expect("history");
        let task = context
            .db
            .tasks()
            .load_task("task-first")
            .await
            .expect("task")
            .expect("present");

        // Assert
        let succeeds = ["questions", "readonly", "stateless"].contains(&scenario);
        assert_eq!(result.is_ok(), succeeds, "{scenario}: {result:?}");
        assert_eq!(response.is_ok(), succeeds, "{scenario}");
        assert_eq!(
            history.is_some(),
            succeeds || ["effects", "unbudgeted-effects"].contains(&scenario),
            "{scenario}"
        );
        assert_eq!(
            task.attempts[0].status,
            if succeeds {
                TaskAttemptStatus::Succeeded
            } else if ["pre-canceled", "cleanup-canceled"].contains(&scenario) {
                TaskAttemptStatus::Canceled
            } else {
                TaskAttemptStatus::Failed
            }
        );
        assert_eq!(
            context
                .db
                .sessions()
                .load_session_messages("sess")
                .await
                .expect("chat")
                .len(),
            0
        );
        assert_eq!(
            context
                .transcript
                .lock()
                .expect("transcript")
                .messages()
                .len(),
            0
        );
    }
}

async fn graph_context(scenario: &'static str) -> SessionWorkerContext {
    let mut context = queue_helper_context(Arc::new(Mutex::new(VecDeque::new()))).await;
    let project = context
        .db
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    context
        .db
        .sessions()
        .insert_session("sess", "gpt-6.1-sol", "main", "Review", project)
        .await
        .expect("session");
    context.folder = "workspace".into();
    context.fs_client = Arc::new(mock_fs_client_with_existing_directories());
    let mut git = MockGitClient::new();
    git.expect_detect_git_info().returning(move |_| {
        Box::pin(async move {
            Some(
                if scenario == "isolation" {
                    "main"
                } else {
                    "wt/sess"
                }
                .into(),
            )
        })
    });
    git.expect_main_checkout_working_tree()
        .returning(|_| Box::pin(async { Ok(None) }));
    git.expect_diff()
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    git.expect_is_worktree_clean().returning(|_| {
        Box::pin(async {
            Err(GitError::CommandFailed {
                command: "status".into(),
                stderr: "index locked".into(),
            })
        })
    });
    context.git_client = Arc::new(git);
    *context.status.lock().expect("status") = Status::Review;

    context
}

async fn graph_task_fixture(
    context: &SessionWorkerContext,
    scenario: &'static str,
) -> (
    GraphTaskCommand,
    oneshot::Receiver<Result<String, ag_contracts::OneShotError>>,
) {
    let mut graph = graph();
    graph.nodes.truncate(1);
    if ["effects", "readonly", "unbudgeted-effects"].contains(&scenario) {
        graph.nodes[0].definition.read_only = false;
        graph.nodes[0].execution.workspace_access = GraphWorkspaceAccess::ExclusiveWrite;
        graph.nodes[0].execution.post_processing = GraphPostProcessing::SessionWorkflow;
    }
    if scenario == "stateless" {
        graph.nodes[0].agent = ag_session::AgentSelection::new(
            ag_session::AgentKind::Claude,
            ag_session::AgentModel::ClaudeSonnet55,
        );
    }
    context
        .db
        .graphs()
        .save_graph(&graph, None)
        .await
        .expect("save graph");
    let dispatch = claimed_dispatch(
        context
            .db
            .graph_dispatch()
            .claim(
                "graph",
                graph.revision,
                "first",
                "aaaaaaaa-0000-0000-0000-000000000001",
                &context.folder,
            )
            .await
            .expect("claim"),
    );
    let operation_id = task_operation_id(&dispatch.record.execution).to_string();
    let cancellation = CancellationToken::new();
    let factory = graph_factory(scenario, &cancellation);
    if scenario == "pre-canceled" {
        cancellation.cancel();
    }
    let (sender, receiver) = oneshot::channel();
    let mut task = GraphTaskCommand {
        cancellation: Arc::new(cancellation),
        completion: GraphTaskCompletion::new(sender),
        factory: Arc::new(factory),
        operation_id,
        request: GraphRunRequest {
            dispatch,
            folder: context.folder.clone(),
            permission_mode: if scenario == "readonly" {
                PermissionMode::ReadOnly
            } else {
                PermissionMode::AutoEdit
            },
            provider_call_budget: (scenario != "unbudgeted-effects")
                .then(|| ProviderCallBudget::new(2)),
        },
    };
    if scenario == "utility" {
        task.request.dispatch.record.execution = ag_session::GraphExecutionRef::UtilityRun {
            run_id: "utility".into(),
        };
    }

    (task, receiver)
}

fn graph_factory(
    scenario: &'static str,
    cancellation: &CancellationToken,
) -> MockSessionRunFactory {
    let mut factory = MockSessionRunFactory::new();
    if scenario == "factory" {
        factory
            .expect_create_graph_task()
            .once()
            .returning(|_, _, _| Err(AgentError::Runtime("factory failed".into())));
    } else if !["pre-canceled", "isolation", "utility", "poisoned-cancel"].contains(&scenario) {
        let mut channel = MockAgentChannel::new();
        channel
            .expect_run_turn()
            .once()
            .returning(move |_, request, _| {
                assert_eq!(request.continuation.provider_conversation_id(), None);
                assert_eq!(request.continuation.replay_transcript(), None);
                Box::pin(async move {
                    if scenario == "provider" {
                        return Err(AgentError::Backend("provider failed".into()));
                    }
                    let mut result = successful_turn_result("task response");
                    if scenario == "questions" {
                        result
                            .assistant_message
                            .questions
                            .push(QuestionItem::new("Task question?"));
                    }
                    result.provider_conversation_id = Some("task-provider".into());
                    Ok(result)
                })
            });
        let cancel = cancellation.clone();
        channel
            .expect_shutdown_session()
            .once()
            .returning(move |_| {
                if scenario == "cleanup-canceled" {
                    cancel.cancel();
                }
                Box::pin(async move {
                    if scenario == "cleanup" {
                        Err(AgentError::Runtime("cleanup failed".into()))
                    } else {
                        Ok(())
                    }
                })
            });
        let client = SessionRunClient::from_channel("isolated-task".into(), Arc::new(channel));
        factory
            .expect_create_graph_task()
            .once()
            .returning(move |_, _, _| Ok(client.clone()));
    }

    factory
}

#[tokio::test]
async fn abandoned_graph_commands_notify_their_waiting_executor() {
    // Arrange
    let (sender, missing_body) = oneshot::channel();
    let completion = GraphTaskCompletion::new(sender);
    completion.finish(Ok(()));
    assert!(missing_body.await.expect("reply").is_err());
    let (sender, receiver) = oneshot::channel();
    let completion = GraphTaskCompletion::new(sender);
    let cancellation = Arc::new(CancellationToken::new());
    let command = ScheduledSessionCommand::immediate(SessionCommand::RunGraphTask(Box::new(
        GraphTaskCommand {
            cancellation: cancellation.clone(),
            completion: completion.clone(),
            factory: Arc::new(MockSessionRunFactory::new()),
            operation_id: "operation".into(),
            request: run_request(),
        },
    )));
    let context = queue_helper_context(Arc::new(Mutex::new(VecDeque::new()))).await;

    // Act
    assert!(!command.can_run_while_question());
    cancellation.cancel();
    assert!(command.can_run_while_question());
    SessionWorkerService::complete_skipped_session_command(&context, &command.command);
    completion.finish(Err("duplicate completion".into()));

    // Assert
    assert!(
        receiver
            .await
            .expect("completion")
            .expect_err("closed")
            .to_string()
            .contains("canceled or already settled")
    );
}
