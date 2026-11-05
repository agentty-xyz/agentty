use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ag_protocol::QuestionItem;
use ag_session::{
    ExecutionGraphStatus, GraphExecutionRef, GraphNodeContinuation, GraphNodeStatus,
    GraphQuestionAnswer, GraphResultState,
};
use ag_store::MockGraphRepository;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::unavailable;
use crate::app::App;
use crate::domain::input::InputState;
use crate::presentation::app_mode::AppMode;
use crate::presentation::execution_graph::tests::graph_view;
use crate::presentation::execution_graph::{GraphEditKind, GraphEditor, GraphView};
use crate::runtime::mode::execution_graph;
use crate::test_support::new_test_app;

#[tokio::test]
async fn rejected_replies_and_answer_sets_require_explicit_reconciliation() {
    // Arrange / Act / Assert
    for kind in [GraphEditKind::Reply, GraphEditKind::Answers] {
        for fault in ["stale", "persistence"] {
            exercise_rejected_draft(kind, fault).await;
        }
    }
}

async fn exercise_rejected_draft(kind: GraphEditKind, fault: &'static str) {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let view = draft_view(kind);
    let mut retained = view.editor.clone().expect("editor");
    retained.needs_reconciliation = true;
    let original_progress = view.progress.clone();
    let mut current = view.progress.clone().expect("progress");
    if fault == "stale" {
        current.nodes[1].last_error = Some("Other branch progressed".into());
    }
    let expected_progress = Some(current.clone());
    let queries = Arc::new(AtomicUsize::new(0));
    let writes = Arc::new(AtomicUsize::new(0));
    let mut repository = MockGraphRepository::new();
    let observed_queries = queries.clone();
    repository.expect_progress().returning(move |_| {
        observed_queries.fetch_add(1, Ordering::SeqCst);
        Ok(Some(current.clone()))
    });
    let graph = view.graphs[0].clone();
    repository
        .expect_list_graphs()
        .returning(move |_| Ok(vec![graph.clone()]));
    repository.expect_load_result().returning(|_| Ok(None));
    let expected_execution = view.node().expect("node").executions[0].clone();
    let expected_kind = draft_continuation(kind);
    let observed_writes = writes.clone();
    repository
        .expect_continue_node()
        .times(if fault == "persistence" { 2 } else { 1 })
        .withf(move |request| {
            request.node_id == "first"
                && request.execution == expected_execution
                && request.kind == expected_kind
        })
        .returning(move |_| {
            let count = observed_writes.fetch_add(1, Ordering::SeqCst);
            if fault == "persistence" && count == 0 {
                Err(unavailable())
            } else {
                Ok(true)
            }
        });
    app.services.set_graph_repository(Arc::new(repository));
    app.mode = AppMode::ExecutionGraph(Box::new(view));

    // Act / Assert: rejection retains the exact editor and original snapshot.
    execution_graph::handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
    assert_eq!(inspector(&app).editor.as_ref(), Some(&retained));
    assert_eq!(inspector(&app).progress, original_progress);
    assert!(inspector(&app).notice.contains("Draft retained"));
    let before_retry = queries.load(Ordering::SeqCst);
    execution_graph::handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
    assert_eq!(queries.load(Ordering::SeqCst), before_retry);
    assert_eq!(inspector(&app).editor.as_ref(), Some(&retained));

    // Act / Assert: refresh is explicit and never submits or retargets the
    // draft.
    execution_graph::handle(
        &mut app,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
    )
    .await;
    retained.needs_reconciliation = false;
    assert_eq!(inspector(&app).editor.as_ref(), Some(&retained));
    assert_eq!(inspector(&app).progress, expected_progress);
    assert_eq!(
        writes.load(Ordering::SeqCst),
        usize::from(fault == "persistence")
    );
    execution_graph::handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
    assert!(inspector(&app).editor.is_none());
    assert!(inspector(&app).notice.contains("Action admitted"));
}

fn draft_view(kind: GraphEditKind) -> GraphView {
    let mut view = graph_view();
    let progress = view.progress.as_mut().expect("progress");
    progress.status = ExecutionGraphStatus::Failed;
    progress.nodes[1].status = GraphNodeStatus::Failed;
    progress.nodes[1].last_error = Some("Previous branch failure".into());
    progress.nodes[1]
        .executions
        .push(GraphExecutionRef::TaskAttempt {
            attempt_id: "other-attempt".into(),
            operation_id: "other-operation".into(),
            task_id: "task-second".into(),
        });
    if kind == GraphEditKind::Answers {
        progress.nodes[0].status = GraphNodeStatus::AwaitingAnswers;
        progress.nodes[0].results[0].state = GraphResultState::Produced;
        view.outputs[0].0.state = GraphResultState::Produced;
        progress.nodes[0].questions = vec![
            QuestionItem::new("Which target?"),
            QuestionItem::new("Which scope?"),
        ];
    }
    view.editor = Some(GraphEditor {
        answers: if kind == GraphEditKind::Answers {
            vec!["Local".into()]
        } else {
            Vec::new()
        },
        input: InputState::with_text(if kind == GraphEditKind::Answers {
            "Docs".into()
        } else {
            "Preserved revision guidance".into()
        }),
        kind,
        needs_reconciliation: false,
    });

    view
}

fn draft_continuation(kind: GraphEditKind) -> GraphNodeContinuation {
    if kind == GraphEditKind::Answers {
        GraphNodeContinuation::Answer {
            answers: vec![
                GraphQuestionAnswer {
                    question: "Which target?".into(),
                    answer: "Local".into(),
                },
                GraphQuestionAnswer {
                    question: "Which scope?".into(),
                    answer: "Docs".into(),
                },
            ],
        }
    } else {
        GraphNodeContinuation::Reply {
            attachments: Vec::new(),
            message: "Preserved revision guidance".into(),
        }
    }
}

fn inspector(app: &App) -> &GraphView {
    match &app.mode {
        AppMode::ExecutionGraph(view) => Some(view.as_ref()),
        _ => None,
    }
    .expect("inspector")
}

#[tokio::test]
async fn failed_refresh_and_superseded_targets_leave_drafts_bound_and_blocked() {
    for fault in ["refresh", "execution", "questions"] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        let mut view = draft_view(GraphEditKind::Answers);
        view.editor.as_mut().expect("editor").needs_reconciliation = true;
        let mut current = view.progress.clone().expect("progress");
        if fault == "execution" {
            let execution = current.nodes[1].executions[0].clone();
            current.nodes[0].executions.push(execution);
        }
        if fault == "questions" {
            current.nodes[0].questions[1].text = "A new question?".into();
        }
        let graph = view.graphs[0].clone();
        let original = view.clone();
        let mut repository = MockGraphRepository::new();
        repository.expect_list_graphs().return_once(move |_| {
            if fault == "refresh" {
                Err(unavailable())
            } else {
                Ok(vec![graph])
            }
        });
        if fault != "refresh" {
            repository
                .expect_progress()
                .return_once(move |_| Ok(Some(current)));
            repository.expect_load_result().return_once(|_| Ok(None));
        }
        app.services.set_graph_repository(Arc::new(repository));
        app.mode = AppMode::ExecutionGraph(Box::new(view));

        // Act
        execution_graph::handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        )
        .await;
        execution_graph::handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;

        // Assert
        assert_eq!(inspector(&app).editor, original.editor);
        assert_eq!(inspector(&app).progress, original.progress);
        assert!(inspector(&app).notice.contains("Ctrl+r"));
    }
}
