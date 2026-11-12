use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use ag_session::{
    ExecutionGraphStatus, GraphExecutionRef, GraphNodeContinuation, QuestionAnswer, QuestionItem,
};
use ag_store::{DbError, MockGraphProposalRepository, MockGraphRepository};
use tempfile::TempDir;

use super::{GraphAnswerOutcome, GraphQuestionTarget};
use crate::app::App;
use crate::app::graph_node_reply::QUESTIONS_NOT_PENDING;
use crate::app::graph_node_reply::tests::{app_with, awaiting_graph, messages};
use crate::app::graph_proposal_reply::tests::{
    apply_next_command, clarification_state, mark_deciding, pending_app, stored, wait_for_message,
};
use crate::domain::input::InputState;
use crate::domain::question::QuestionProgress;
use crate::domain::session::{SessionId, Status};
use crate::presentation::app_mode::AppMode;

#[tokio::test]
async fn node_questions_open_the_panel_and_answers_reach_the_asking_execution() {
    // Arrange
    let (mut repository, execution) = awaiting_node_repository(usize::MAX);
    let expected_execution = execution.clone();
    repository
        .expect_continue_node()
        .once()
        .withf(move |request| {
            request.execution == expected_execution
                && request.kind
                    == GraphNodeContinuation::Reply {
                        attachments: Vec::new(),
                        message: "Clarifications:\n1. Q: Which update?\n   A: Docs".into(),
                    }
        })
        .returning(|_| Ok(true));
    let (mut app, _directory) = viewing_app(repository).await;
    let target = GraphQuestionTarget::Node(execution);

    // Act
    let opened = app.reconcile_graph_question_mode().await;
    let active = app.graph_questions.active_target("sess").cloned();
    let panel_questions = question_panel(&app);
    let outcome = app
        .answer_graph_questions(&session(), &target, &answers("Which update?", "Docs"))
        .await;

    // Assert
    assert!(opened);
    assert_eq!(active, Some(target));
    assert_eq!(
        panel_questions,
        Some(vec![QuestionItem::new("Which update?")])
    );
    assert_eq!(outcome, GraphAnswerOutcome::Sent);
    assert_eq!(app.graph_questions.active_target("sess"), None);
    assert!(messages(&app).await.contains(&(
        "user_prompt".to_string(),
        "Clarifications:\n1. Q: Which update?\n   A: Docs".to_string()
    )));
}

#[tokio::test]
async fn dismissed_graph_questions_stay_closed_until_the_session_is_reopened() {
    // Arrange
    let (repository, _execution) = awaiting_node_repository(usize::MAX);
    let (mut app, _directory) = viewing_app(repository).await;
    app.reconcile_graph_question_mode().await;

    // Act
    let dismissed = app.dismiss_graph_questions(&session());
    app.graph_questions.checked = None;
    let reopened_while_dismissed = app.reconcile_graph_question_mode().await;
    app.graph_questions.forget_dismissal("sess");
    app.graph_questions.checked = None;
    let reopened_after_reopen = app.reconcile_graph_question_mode().await;
    app.graph_questions.clear_active();
    let dismissed_ordinary = app.dismiss_graph_questions(&session());

    // Assert
    assert!(dismissed);
    assert!(!reopened_while_dismissed);
    assert!(reopened_after_reopen);
    assert!(!dismissed_ordinary);
    assert!(matches!(app.mode, AppMode::Question { .. }));
}

#[tokio::test]
async fn graph_questions_open_only_from_an_unchecked_plain_view_without_turn_questions() {
    // Arrange
    let (repository, _execution) = awaiting_node_repository(usize::MAX);
    let (mut app, _directory) = viewing_app(repository).await;

    // Act
    app.mode = AppMode::List;
    let from_list = app.reconcile_graph_question_mode().await;
    app.mode = view_mode("other");
    let from_unknown_session = app.reconcile_graph_question_mode().await;
    app.mode = view_mode("sess");
    set_status(&mut app, Status::Question);
    let with_turn_questions = app.reconcile_graph_question_mode().await;
    set_status(&mut app, Status::Done);
    let when_done = app.reconcile_graph_question_mode().await;
    set_status(&mut app, Status::Canceled);
    let when_canceled = app.reconcile_graph_question_mode().await;
    set_status(&mut app, Status::Review);
    let first_check = app.reconcile_graph_question_mode().await;
    app.mode = view_mode("sess");
    let recent_check = app.reconcile_graph_question_mode().await;

    // Assert
    assert!(!from_list);
    assert!(!from_unknown_session);
    assert!(!with_turn_questions);
    assert!(!when_done);
    assert!(!when_canceled);
    assert!(first_check);
    assert!(!recent_check);
}

#[tokio::test]
async fn saved_answers_reopen_only_for_the_graph_work_they_answered() {
    // Arrange
    let (repository, execution) = awaiting_node_repository(usize::MAX);
    let (mut app, _directory) = viewing_app(repository).await;
    let node = Some(GraphQuestionTarget::Node(execution));
    let proposal = Some(GraphQuestionTarget::Proposal {
        proposal_id: "proposal".into(),
        version: 1,
    });

    // Act
    save_first_answer(&mut app, node.clone());
    app.open_question_panel("sess", two_questions(), proposal);
    let other_graph_work_index = question_index(&app);
    save_first_answer(&mut app, node.clone());
    app.enter_question_mode("sess", two_questions());
    let ordinary_turn_index = question_index(&app);
    save_first_answer(&mut app, None);
    app.open_question_panel("sess", two_questions(), node.clone());
    let ordinary_answers_in_graph_index = question_index(&app);
    save_first_answer(&mut app, node.clone());
    app.open_question_panel("sess", two_questions(), node.clone());
    let same_graph_work_index = question_index(&app);

    // Assert
    assert_eq!(other_graph_work_index, Some(0));
    assert_eq!(ordinary_turn_index, Some(0));
    assert_eq!(ordinary_answers_in_graph_index, Some(0));
    assert_eq!(same_graph_work_index, Some(1));
    assert_eq!(app.graph_questions.active_target("sess"), node.as_ref());
    assert!(app.question_progress.is_empty());
}

#[tokio::test]
async fn failed_graph_lookups_leave_the_view_unchanged() {
    // Arrange
    let mut repository = MockGraphRepository::new();
    repository
        .expect_list_graphs()
        .returning(|_| Err(unavailable()));
    let (mut graph_app, _graph_directory) = viewing_app(repository).await;
    let (mut proposal_app, _proposal_directory) = failing_proposal_app().await;

    // Act
    let graph_opened = graph_app.reconcile_graph_question_mode().await;
    let proposal_opened = proposal_app.reconcile_graph_question_mode().await;

    // Assert
    assert!(!graph_opened);
    assert!(!proposal_opened);
    assert!(matches!(graph_app.mode, AppMode::View { .. }));
    assert!(matches!(proposal_app.mode, AppMode::View { .. }));
}

#[tokio::test]
async fn stale_node_answers_release_the_panel_target_and_failed_lookups_keep_it() {
    // Arrange
    let (repository, execution) = awaiting_node_repository(0);
    let (mut app, _directory) = viewing_app(repository).await;
    let mut failing = MockGraphRepository::new();
    failing
        .expect_list_graphs()
        .returning(|_| Err(unavailable()));
    let (mut failing_app, _failing_directory) = viewing_app(failing).await;
    let target = GraphQuestionTarget::Node(execution);
    let answers = answers("Which update?", "Docs");
    app.graph_questions.active = Some((session(), target.clone()));
    app.graph_questions.checked = Some((session(), 0, Instant::now()));
    failing_app.graph_questions.active = Some((session(), target.clone()));

    // Act
    let answered = app
        .answer_graph_questions(&session(), &target, &answers)
        .await;
    let failed = failing_app
        .answer_graph_questions(&session(), &target, &answers)
        .await;

    // Assert
    assert_eq!(answered, GraphAnswerOutcome::Stale);
    assert_eq!(failed, GraphAnswerOutcome::Retry);
    assert_eq!(app.graph_questions.active_target("sess"), None);
    assert_eq!(app.graph_questions.checked, None);
    assert_eq!(
        failing_app.graph_questions.active_target("sess"),
        Some(&target)
    );
    assert_eq!(
        messages(&app).await,
        [(
            "workflow_notice".to_string(),
            format!("\n[Reply Error] Graph answer: {QUESTIONS_NOT_PENDING}\n")
        )]
    );
    assert!(
        messages(&failing_app)
            .await
            .iter()
            .any(|(_, content)| content.contains("Read failed"))
    );
}

#[tokio::test]
async fn proposal_clarifications_open_the_panel_until_a_reply_is_being_decided() {
    // Arrange
    let (mut app, _directory) = pending_app(clarification_state(), "revise").await;
    app.mode = view_mode("sess");

    // Act
    let opened = app.reconcile_graph_question_mode().await;
    let active = app.graph_questions.active_target("sess").cloned();
    let panel_questions = question_panel(&app);
    mark_deciding(&app, "preview");
    app.mode = view_mode("sess");
    app.graph_questions.checked = None;
    let opened_while_deciding = app.reconcile_graph_question_mode().await;

    // Assert
    assert!(opened);
    assert_eq!(
        active,
        Some(GraphQuestionTarget::Proposal {
            proposal_id: "preview".into(),
            version: 1,
        })
    );
    assert_eq!(
        panel_questions,
        Some(vec![QuestionItem::new("Which target?")])
    );
    assert!(!opened_while_deciding);
}

#[tokio::test]
async fn proposal_answers_revise_the_inspected_version_only() {
    // Arrange
    let (mut app, _directory) = pending_app(clarification_state(), "revise").await;
    let _consumer = app.sessions.foreground_consumer();
    let answers = answers("Which target?", "The local target");
    let stale = GraphQuestionTarget::Proposal {
        proposal_id: "preview".into(),
        version: 2,
    };
    let inspected = GraphQuestionTarget::Proposal {
        proposal_id: "preview".into(),
        version: 1,
    };

    // Act
    let stale_outcome = app
        .answer_graph_questions(&session(), &stale, &answers)
        .await;
    let outcome = app
        .answer_graph_questions(&session(), &inspected, &answers)
        .await;
    apply_next_command(&mut app).await;
    wait_for_message(&app, "Generating graph proposal v2…").await;
    app.services.wait_for_cleanup_tasks(None).await;

    // Assert
    assert_eq!(stale_outcome, GraphAnswerOutcome::Stale);
    assert_eq!(outcome, GraphAnswerOutcome::Sent);
    assert_eq!(stored(&app).await.version.number, 2);
    let messages = messages(&app).await;
    assert!(messages.contains(&(
        "workflow_notice".to_string(),
        format!("\n[Reply Error] {QUESTIONS_NOT_PENDING}\n")
    )));
    assert!(messages.contains(&(
        "user_prompt".to_string(),
        "Clarifications:\n1. Q: Which target?\n   A: The local target".to_string()
    )));
}

#[tokio::test]
async fn failed_proposal_lookup_rejects_answers() {
    // Arrange
    let (mut app, _directory) = failing_proposal_app().await;
    let target = GraphQuestionTarget::Proposal {
        proposal_id: "preview".into(),
        version: 1,
    };

    // Act
    let outcome = app
        .answer_graph_questions(&session(), &target, &answers("Which target?", "Local"))
        .await;

    // Assert
    assert_eq!(outcome, GraphAnswerOutcome::Retry);
    assert!(
        messages(&app)
            .await
            .iter()
            .any(|(_, content)| content.contains("read unavailable"))
    );
}

/// Graph repository whose single node waits for answers to "Which update?"
/// during the first `waiting_lookups` graph lookups and has no graphs after.
pub(crate) fn awaiting_node_repository(
    waiting_lookups: usize,
) -> (MockGraphRepository, GraphExecutionRef) {
    let (graph, progress) = awaiting_graph("graph", ExecutionGraphStatus::Running);
    let execution = progress.nodes[0].executions[0].clone();
    let lookups = AtomicUsize::new(0);
    let mut repository = MockGraphRepository::new();
    repository.expect_list_graphs().returning(move |_| {
        let is_waiting = lookups.fetch_add(1, Ordering::SeqCst) < waiting_lookups;

        Ok(if is_waiting {
            vec![graph.clone()]
        } else {
            Vec::new()
        })
    });
    repository
        .expect_progress()
        .returning(move |_| Ok(Some(progress.clone())));

    (repository, execution)
}

/// App viewing the seeded `sess` chat with graph persistence from
/// `repository`.
pub(crate) async fn viewing_app(repository: MockGraphRepository) -> (App, TempDir) {
    let (mut app, directory) = app_with(repository).await;
    app.mode = view_mode("sess");

    (app, directory)
}

/// App viewing `sess` with proposals enabled and every proposal read failing.
async fn failing_proposal_app() -> (App, TempDir) {
    let (mut app, directory) = viewing_app(MockGraphRepository::new()).await;
    app.experimental_graph = true;
    let mut proposals = MockGraphProposalRepository::new();
    proposals.expect_latest().returning(|_| {
        Err(DbError::InvalidData {
            entity: "proposal",
            reason: "read unavailable".into(),
        })
    });
    app.services
        .set_graph_proposal_repository(Arc::new(proposals));

    (app, directory)
}

fn answers(question: &str, answer: &str) -> Vec<QuestionAnswer> {
    vec![QuestionAnswer {
        answer: answer.into(),
        question: question.into(),
    }]
}

/// Opens the `sess` panel for `target` and saves its first answer as a `q`
/// exit does.
fn save_first_answer(app: &mut App, target: Option<GraphQuestionTarget>) {
    app.open_question_panel("sess", two_questions(), target);
    app.graph_questions.record_progress_target("sess");
    app.question_progress.insert(
        session(),
        QuestionProgress {
            current_index: 1,
            input: InputState::default(),
            responses: vec!["First answer".into()],
            selected_option_index: None,
        },
    );
    app.mode = AppMode::List;
}

fn two_questions() -> Vec<QuestionItem> {
    vec![QuestionItem::new("First?"), QuestionItem::new("Second?")]
}

fn question_index(app: &App) -> Option<usize> {
    match &app.mode {
        AppMode::Question { current_index, .. } => Some(*current_index),
        _ => None,
    }
}

fn question_panel(app: &App) -> Option<Vec<QuestionItem>> {
    match &app.mode {
        AppMode::Question {
            questions,
            session_id,
            ..
        } if session_id == "sess" => Some(questions.clone()),
        _ => None,
    }
}

fn session() -> SessionId {
    SessionId::from("sess")
}

fn set_status(app: &mut App, status: Status) {
    if let Some(session) = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == "sess")
    {
        session.status = status;
    }
}

/// Graph store failure reported while reading graph state.
pub(crate) fn unavailable() -> DbError {
    DbError::InvalidData {
        entity: "graph",
        reason: "Read failed".into(),
    }
}

fn view_mode(session_id: &str) -> AppMode {
    AppMode::View {
        session_id: session_id.into(),
        scroll_offset: None,
    }
}
