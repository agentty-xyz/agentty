//! Session API graph question tests: the clarification questions of a
//! session's graph work in its snapshot, and complete answer sets routed to
//! the graph work that asked them.

use std::sync::atomic::{AtomicUsize, Ordering};

use ag_session::{
    AnswerQuestionsRequest, ExecutionGraphStatus, GraphNodeContinuation, QuestionAnswer,
    QuestionItem, SessionError as ApiSessionError, SessionId,
};
use ag_store::MockGraphRepository;
use tempfile::TempDir;

use super::support::{request_question_answers, request_session};
use crate::app::App;
use crate::app::graph_node_reply::QUESTIONS_NOT_PENDING;
use crate::app::graph_node_reply::tests::{app_with, awaiting_graph, messages};
use crate::app::graph_proposal_reply::tests::{clarification_state, pending_app};
use crate::app::graph_question::GraphQuestionTarget;
use crate::app::graph_question::tests::{awaiting_node_repository, unavailable};

/// App whose `sess` asks no turn questions while its graph node waits for
/// "Which update?" during the first `waiting_lookups` graph lookups.
async fn node_app(waiting_lookups: usize) -> (App, TempDir) {
    let (repository, _execution) = awaiting_node_repository(waiting_lookups);
    let (app, directory) = app_with(repository).await;
    clear_turn_questions(&app).await;

    (app, directory)
}

/// Removes the turn questions the seeded `sess` asks.
async fn clear_turn_questions(app: &App) {
    app.services
        .db()
        .sessions()
        .update_session_questions("sess", "")
        .await
        .expect("questions should clear");
}

/// Stores `status` as the persisted status the snapshot of `sess` reports.
async fn store_status(app: &App, status: &str) {
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at("sess", status, 0)
        .await
        .expect("status should store");
}

/// The questions the snapshot of `sess` reports, and the key of the graph
/// work that asks them.
async fn snapshot_questions(app: &mut App) -> (Vec<QuestionItem>, Option<String>) {
    let session = request_session(app, SessionId::from("sess"))
        .await
        .expect("snapshot should load")
        .expect("session should exist");

    (session.questions, session.graph_question_target)
}

/// The key of version `version` of the `preview` proposal.
fn proposal_key(version: u32) -> String {
    GraphQuestionTarget::Proposal {
        proposal_id: "preview".into(),
        version,
    }
    .key()
}

/// The key of the waiting execution of the node `awaiting_graph` shows.
fn node_key() -> String {
    let (_graph, progress) = awaiting_graph("graph", ExecutionGraphStatus::Running);

    GraphQuestionTarget::Node(progress.nodes[0].executions[0].clone()).key()
}

/// Answers `question` of `sess` with `answer` through the session API, for
/// the graph work `target` names, or for the turn without one.
async fn answer(
    app: &mut App,
    target: Option<&str>,
    question: &str,
    answer: &str,
) -> Result<(), ApiSessionError> {
    request_question_answers(
        app,
        SessionId::from("sess"),
        AnswerQuestionsRequest {
            answers: vec![QuestionAnswer {
                answer: answer.into(),
                question: question.into(),
            }],
            attachments: Vec::new(),
            graph_question_target: target.map(str::to_string),
        },
    )
    .await
}

/// Graph repository whose node waits for "Which update?" in the first
/// lookup and whose later lookups fail.
fn failing_after_first_lookup() -> MockGraphRepository {
    let (graph, progress) = awaiting_graph("graph", ExecutionGraphStatus::Running);
    let lookups = AtomicUsize::new(0);
    let mut repository = MockGraphRepository::new();
    repository.expect_list_graphs().returning(move |_| {
        if lookups.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(vec![graph.clone()])
        } else {
            Err(unavailable())
        }
    });
    repository
        .expect_progress()
        .returning(move |_| Ok(Some(progress.clone())));

    repository
}

#[tokio::test]
async fn snapshots_report_proposal_clarifications_only_while_the_turn_asks_none() {
    // Arrange
    let (mut app, _directory) = pending_app(clarification_state(), "revise").await;

    // Act
    let with_turn_questions = snapshot_questions(&mut app).await;
    clear_turn_questions(&app).await;
    let without_turn_questions = snapshot_questions(&mut app).await;

    // Assert
    assert_eq!(
        with_turn_questions,
        (vec![QuestionItem::new("ordinary question")], None)
    );
    assert_eq!(
        without_turn_questions,
        (
            vec![QuestionItem::new("Which target?")],
            Some(proposal_key(1))
        )
    );
}

#[test]
fn graph_question_keys_tell_apart_new_graph_work_asking_the_same_questions() {
    // Arrange
    let (_graph, progress) = awaiting_graph("graph", ExecutionGraphStatus::Running);
    let node = GraphQuestionTarget::Node(progress.nodes[0].executions[0].clone());

    // Act
    let keys = [proposal_key(1), proposal_key(2), node.key()];

    // Assert
    assert_eq!(keys[0], proposal_key(1));
    assert_ne!(keys[0], keys[1]);
    assert_ne!(keys[0], keys[2]);
    assert_ne!(keys[1], keys[2]);
    assert!(
        keys.iter()
            .all(|key| key.len() == 16 && !key.contains("preview")),
        "{keys:?}"
    );
}

#[tokio::test]
async fn snapshots_report_node_questions_except_where_the_terminal_shows_none() {
    // Arrange
    let (repository, execution) = awaiting_node_repository(usize::MAX);
    let (mut app, _directory) = app_with(repository).await;
    clear_turn_questions(&app).await;
    let (mut failing, _failing_directory) = app_with(failing_after_first_lookup()).await;
    clear_turn_questions(&failing).await;
    snapshot_questions(&mut failing).await;

    // Act
    let in_review = snapshot_questions(&mut app).await;
    let mut excluded = Vec::new();
    for status in ["Question", "Done", "Canceled"] {
        store_status(&app, status).await;
        excluded.push(snapshot_questions(&mut app).await);
    }
    let after_failed_lookup = snapshot_questions(&mut failing).await;

    // Assert
    assert_eq!(
        in_review,
        (
            vec![QuestionItem::new("Which update?")],
            Some(GraphQuestionTarget::Node(execution).key())
        )
    );
    assert_eq!(excluded, vec![(Vec::new(), None); 3]);
    assert_eq!(after_failed_lookup, (Vec::new(), None));
}

#[tokio::test]
async fn answers_reach_the_graph_node_execution_that_asked() {
    // Arrange
    let (mut repository, execution) = awaiting_node_repository(usize::MAX);
    repository
        .expect_continue_node()
        .once()
        .withf(move |request| {
            request.execution == execution
                && request.kind
                    == GraphNodeContinuation::Reply {
                        attachments: Vec::new(),
                        message: "Clarifications:\n1. Q: Which update?\n   A: Docs".into(),
                    }
        })
        .returning(|_| Ok(true));
    let (mut app, _directory) = app_with(repository).await;
    clear_turn_questions(&app).await;

    // Act
    let answered = answer(&mut app, Some(&node_key()), "Which update?", "Docs").await;

    // Assert
    assert_eq!(answered, Ok(()));
    assert!(messages(&app).await.contains(&(
        "user_prompt".to_string(),
        "Clarifications:\n1. Q: Which update?\n   A: Docs".to_string()
    )));
}

#[tokio::test]
async fn answers_reach_the_proposal_version_that_asked() {
    // Arrange
    let (mut app, _directory) = pending_app(clarification_state(), "revise").await;
    clear_turn_questions(&app).await;

    // Act
    let answered = answer(
        &mut app,
        Some(&proposal_key(1)),
        "Which target?",
        "The local target",
    )
    .await;

    // Assert
    assert_eq!(answered, Ok(()));
    let replies = app
        .services
        .db()
        .graph_proposals()
        .unsettled_replies(&SessionId::from("sess"))
        .await
        .expect("replies should load");
    assert_eq!(
        replies
            .iter()
            .map(|reply| (
                reply.proposal_id.as_str(),
                reply.version,
                reply.message.as_str()
            ))
            .collect::<Vec<_>>(),
        [(
            "preview",
            1,
            "Clarifications:\n1. Q: Which target?\n   A: The local target"
        )]
    );
}

#[tokio::test]
async fn answers_must_match_the_graph_questions_asked_now() {
    // Arrange
    let (mut app, _directory) = node_app(usize::MAX).await;
    let (mut idle, _idle_directory) = node_app(0).await;
    let key = node_key();

    // Act
    let mismatched = answer(&mut app, Some(&key), "Which branch?", "main").await;
    let blank = answer(&mut app, Some(&key), "Which update?", " ").await;
    let unasked = answer(&mut idle, Some(&key), "Which update?", "Docs").await;
    // A page that names no graph work answers only the turn's questions.
    let untargeted = answer(&mut app, None, "Which update?", "Docs").await;

    // Assert
    assert_eq!(
        mismatched,
        Err(ApiSessionError::Operation(
            "Question answer 1 is stale".to_string()
        ))
    );
    assert_eq!(
        blank,
        Err(ApiSessionError::Operation(
            "Question answer 1 is empty".to_string()
        ))
    );
    assert_eq!(
        unasked,
        Err(ApiSessionError::Operation(
            QUESTIONS_NOT_PENDING.to_string()
        ))
    );
    assert_eq!(
        untargeted,
        Err(ApiSessionError::Operation(
            "Session has no questions to answer".to_string()
        ))
    );
}

#[tokio::test]
async fn answers_for_other_graph_work_asking_the_same_questions_are_refused() {
    // Arrange
    let (mut app, _directory) = pending_app(clarification_state(), "revise").await;
    let (mut turn, _turn_directory) = pending_app(clarification_state(), "revise").await;
    clear_turn_questions(&app).await;

    // Act
    let other_version = answer(&mut app, Some(&proposal_key(2)), "Which target?", "Local").await;
    let other_work = answer(&mut app, Some(&node_key()), "Which target?", "Local").await;
    let while_the_turn_asks =
        answer(&mut turn, Some(&proposal_key(1)), "Which target?", "Local").await;

    // Assert
    let stale = Err(ApiSessionError::Operation(
        QUESTIONS_NOT_PENDING.to_string(),
    ));
    assert_eq!(other_version, stale);
    assert_eq!(other_work, stale);
    assert_eq!(while_the_turn_asks, stale);
    let replies: Vec<String> = app
        .services
        .db()
        .graph_proposals()
        .unsettled_replies(&SessionId::from("sess"))
        .await
        .expect("replies should load")
        .into_iter()
        .map(|reply| reply.message)
        .collect();
    assert_eq!(replies, Vec::<String>::new());
}

#[tokio::test]
async fn answers_the_graph_work_no_longer_takes_are_refused() {
    // Arrange
    let (mut settled, _directory) = node_app(1).await;
    let (mut failing, _failing_directory) = app_with(failing_after_first_lookup()).await;
    clear_turn_questions(&failing).await;
    let (mut unreadable, _unreadable_directory) = app_with(failing_after_first_lookup()).await;
    clear_turn_questions(&unreadable).await;
    snapshot_questions(&mut unreadable).await;
    let (mut refusing_repository, _execution) = awaiting_node_repository(usize::MAX);
    refusing_repository
        .expect_continue_node()
        .returning(|_| Err(unavailable()));
    let (mut refusing, _refusing_directory) = app_with(refusing_repository).await;
    clear_turn_questions(&refusing).await;

    // Act
    let key = node_key();
    let stale = answer(&mut settled, Some(&key), "Which update?", "Docs").await;
    let retry = answer(&mut failing, Some(&key), "Which update?", "Docs").await;
    let lookup_failed = answer(&mut unreadable, Some(&key), "Which update?", "Docs").await;
    let refused = answer(&mut refusing, Some(&key), "Which update?", "Docs").await;

    // Assert
    assert_eq!(
        stale,
        Err(ApiSessionError::Operation(
            QUESTIONS_NOT_PENDING.to_string()
        ))
    );
    let not_taken = Err(ApiSessionError::Operation(
        "The graph work did not take the answers; the chat says why. Try again.".to_string(),
    ));
    assert_eq!(retry, not_taken);
    assert_eq!(refused, not_taken);
    assert!(
        matches!(&lookup_failed, Err(ApiSessionError::Operation(message)) if message.contains("Read failed")),
        "{lookup_failed:?}"
    );
}
