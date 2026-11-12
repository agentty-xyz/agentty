use std::path::PathBuf;
use std::sync::Arc;

use ag_protocol::{QuestionItem, TurnPrompt, TurnPromptAttachment};
use ag_session::{
    ExecutionGraph, ExecutionGraphProgress, ExecutionGraphStatus, GraphContext, GraphExecutionRef,
    GraphNodeContinuation, GraphNodeStatus,
};
use ag_store::{DbError, MockGraphRepository};
use tempfile::TempDir;

use crate::app::App;
use crate::app::execution_graph::tests::seed_session;
use crate::app::prompt_intent::PromptWorkflowOutcome;
use crate::domain::session::SessionId;
use crate::presentation::execution_graph::tests::graph_view;
use crate::test_support::new_test_app;

#[tokio::test]
async fn chat_reply_answers_the_awaiting_node_of_a_paused_graph() {
    // Arrange
    let (graph, progress) = awaiting_graph("graph", ExecutionGraphStatus::Blocked);
    let expected_execution = progress.nodes[0].executions[0].clone();
    let mut repository = MockGraphRepository::new();
    repository
        .expect_list_graphs()
        .returning(move |_| Ok(vec![graph.clone()]));
    repository
        .expect_progress()
        .returning(move |_| Ok(Some(progress.clone())));
    repository
        .expect_continue_node()
        .once()
        .withf(move |request| {
            request.graph_id == "graph"
                && request.node_id == "first"
                && request.execution == expected_execution
                && request.kind
                    == GraphNodeContinuation::Reply {
                        attachments: vec![screenshot()],
                        message: "Only small fixes [Image #1]".into(),
                    }
        })
        .returning(|_| Ok(true));
    let (mut app, _directory) = app_with(repository).await;
    let prompt = TurnPrompt {
        attachments: vec![screenshot()],
        ..TurnPrompt::from_text("Only small fixes [Image #1]".into())
    };

    // Act
    let outcome = app
        .submit_graph_node_reply(&SessionId::from("sess"), &prompt)
        .await;

    // Assert
    assert_eq!(
        outcome,
        Some(PromptWorkflowOutcome::ShowSession {
            session_id: "sess".into()
        })
    );
    assert_eq!(
        messages(&app).await,
        [
            (
                "user_prompt".to_string(),
                "Only small fixes [Image #1]".to_string()
            ),
            (
                "workflow_notice".to_string(),
                "Answer sent to `first`. The graph is paused; resume it from `w` to continue."
                    .to_string()
            ),
        ]
    );
}

#[tokio::test]
async fn chat_reply_answers_the_question_asked_last_when_several_nodes_wait() {
    // Arrange
    let view = graph_view();
    let graph = view.graphs[0].clone();
    let mut progress = view.progress.expect("progress");
    progress.status = ExecutionGraphStatus::Running;
    let mut second_execution = progress.nodes[0].executions[0].clone();
    if let GraphExecutionRef::TaskAttempt { attempt_id, .. } = &mut second_execution {
        *attempt_id = "attempt-second".into();
    }
    progress.nodes[1].executions = vec![second_execution.clone()];
    for node in &mut progress.nodes {
        node.status = GraphNodeStatus::AwaitingAnswers;
        node.questions = vec![QuestionItem::new("Which update?")];
    }
    let (older, older_progress) = awaiting_graph("older", ExecutionGraphStatus::Running);
    let mut older_execution = older_progress.nodes[0].executions[0].clone();
    if let GraphExecutionRef::TaskAttempt { attempt_id, .. } = &mut older_execution {
        *attempt_id = "attempt-older".into();
    }
    let mut older_progress = older_progress;
    older_progress.nodes[0].executions = vec![older_execution.clone()];
    let expected_execution = second_execution.clone();
    let mut repository = MockGraphRepository::new();
    repository
        .expect_list_graphs()
        .return_once(move |_| Ok(vec![graph, older]));
    repository
        .expect_progress()
        .returning(move |graph_id| match graph_id {
            "older" => Ok(Some(older_progress.clone())),
            _ => Ok(Some(progress.clone())),
        });
    repository
        .expect_task_question_message_id()
        .times(3)
        .returning(move |execution| {
            Ok(if *execution == second_execution {
                Some(3)
            } else if *execution == older_execution {
                None
            } else {
                Some(1)
            })
        });
    repository
        .expect_continue_node()
        .once()
        .withf(move |request| {
            request.graph_id == "graph"
                && request.node_id == "second"
                && request.execution == expected_execution
        })
        .returning(|_| Ok(true));
    let (mut app, _directory) = app_with(repository).await;

    // Act
    let outcome = app
        .submit_graph_node_reply(
            &SessionId::from("sess"),
            &TurnPrompt::from_text("Docs only".into()),
        )
        .await;

    // Assert
    assert_eq!(
        outcome,
        Some(PromptWorkflowOutcome::ShowSession {
            session_id: "sess".into()
        })
    );
    assert_eq!(
        messages(&app).await,
        [
            ("user_prompt".to_string(), "Docs only".to_string()),
            (
                "workflow_notice".to_string(),
                "Answer sent to `second`. The graph is paused; resume it from `w` to continue."
                    .to_string()
            ),
        ]
    );
}

#[tokio::test]
async fn chat_reply_without_loaded_session_handles_is_admitted_without_chat_echo() {
    // Arrange
    let (graph, progress) = awaiting_graph("graph", ExecutionGraphStatus::Running);
    let mut repository = MockGraphRepository::new();
    repository
        .expect_list_graphs()
        .return_once(move |_| Ok(vec![graph]));
    repository
        .expect_progress()
        .returning(move |_| Ok(Some(progress.clone())));
    repository
        .expect_continue_node()
        .once()
        .withf(|request| request.graph_id == "graph" && request.node_id == "first")
        .returning(|_| Ok(true));
    let (mut app, _directory) = app_with(repository).await;
    let unloaded = SessionId::from("unloaded");
    assert!(app.sessions.session_handles_or_err(&unloaded).is_err());

    // Act
    let outcome = app
        .submit_graph_node_reply(&unloaded, &TurnPrompt::from_text("Docs only".into()))
        .await;

    // Assert
    assert_eq!(
        outcome,
        Some(PromptWorkflowOutcome::ShowSession {
            session_id: unloaded
        })
    );
    assert_eq!(messages(&app).await, Vec::<(String, String)>::new());
}

#[tokio::test]
async fn sessions_without_an_answerable_node_keep_ordinary_routing() {
    // Arrange
    let (mut fresh, fresh_progress) = awaiting_graph("fresh", ExecutionGraphStatus::Blocked);
    fresh.nodes[0].execution.context = GraphContext::Fresh;
    let (missing, _) = awaiting_graph("missing", ExecutionGraphStatus::Blocked);
    let (cancelled, cancelled_progress) =
        awaiting_graph("cancelled", ExecutionGraphStatus::Cancelled);
    let (settled, mut settled_progress) = awaiting_graph("settled", ExecutionGraphStatus::Blocked);
    settled_progress.nodes[0].status = GraphNodeStatus::Succeeded;
    let mut repository = MockGraphRepository::new();
    repository
        .expect_list_graphs()
        .return_once(move |_| Ok(vec![fresh, missing, cancelled, settled]));
    repository
        .expect_progress()
        .returning(move |graph_id| match graph_id {
            "fresh" => Ok(Some(fresh_progress.clone())),
            "cancelled" => Ok(Some(cancelled_progress.clone())),
            "settled" => Ok(Some(settled_progress.clone())),
            _ => Ok(None),
        });
    repository.expect_continue_node().never();
    let (mut app, _directory) = app_with(repository).await;

    // Act
    let outcome = app
        .submit_graph_node_reply(
            &SessionId::from("sess"),
            &TurnPrompt::from_text("An ordinary message".into()),
        )
        .await;

    // Assert
    assert_eq!(outcome, None);
    assert_eq!(messages(&app).await, Vec::<(String, String)>::new());
}

#[tokio::test]
async fn failed_lookup_or_admission_keeps_the_draft_and_reports_why() {
    for fault in ["list", "progress", "question", "admission"] {
        // Arrange
        let (graph, progress) = awaiting_graph("graph", ExecutionGraphStatus::Running);
        let (other, _) = awaiting_graph("other", ExecutionGraphStatus::Running);
        let mut repository = MockGraphRepository::new();
        repository.expect_list_graphs().return_once(move |_| {
            if fault == "list" {
                Err(unavailable())
            } else {
                Ok(vec![graph, other])
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
            .expect_task_question_message_id()
            .returning(move |_| {
                if fault == "question" {
                    Err(unavailable())
                } else {
                    Ok(None)
                }
            });
        repository
            .expect_continue_node()
            .returning(|_| Err(unavailable()));
        let (mut app, _directory) = app_with(repository).await;

        // Act
        let outcome = app
            .submit_graph_node_reply(
                &SessionId::from("sess"),
                &TurnPrompt::from_text("Local only".into()),
            )
            .await;

        // Assert
        assert_eq!(outcome, Some(PromptWorkflowOutcome::KeepPrompt), "{fault}");
        let messages = messages(&app).await;
        assert_eq!(messages.len(), 1, "{fault}: {messages:?}");
        let (kind, content) = &messages[0];
        assert_ne!(kind, "user_prompt", "{fault}: {messages:?}");
        assert!(
            content.contains("Graph answer:") && content.contains("Read failed"),
            "{fault}: {messages:?}"
        );
    }
}

/// One-node graph whose node asked a question, with the given aggregate
/// status before driver overlays.
pub(in crate::app) fn awaiting_graph(
    id: &str,
    status: ExecutionGraphStatus,
) -> (ExecutionGraph, ExecutionGraphProgress) {
    let view = graph_view();
    let mut graph = view.graphs[0].clone();
    graph.id = id.into();
    graph.nodes.truncate(1);
    let mut progress = view.progress.expect("progress");
    progress.graph_id = id.into();
    progress.status = status;
    progress.nodes.truncate(1);
    progress.nodes[0].status = GraphNodeStatus::AwaitingAnswers;
    progress.nodes[0].questions = vec![QuestionItem::new("Which update?")];

    (graph, progress)
}

pub(in crate::app) async fn app_with(repository: MockGraphRepository) -> (App, TempDir) {
    let (mut app, directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    app.services.set_graph_repository(Arc::new(repository));

    (app, directory)
}

pub(crate) fn screenshot() -> TurnPromptAttachment {
    TurnPromptAttachment {
        local_image_path: PathBuf::from("screenshot.png"),
        placeholder: "[Image #1]".into(),
    }
}

fn unavailable() -> DbError {
    DbError::InvalidData {
        entity: "graph",
        reason: "Read failed".into(),
    }
}

pub(in crate::app) async fn messages(app: &App) -> Vec<(String, String)> {
    app.services
        .db()
        .sessions()
        .load_session_messages("sess")
        .await
        .expect("messages")
        .into_iter()
        .map(|message| (message.kind, message.content))
        .collect()
}
