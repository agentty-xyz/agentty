use ag_protocol::QuestionItem;
use ag_session::{
    AnswerQuestionsRequest, CreateSessionMode, CreateSessionRequest, QuestionAnswer,
    SessionError as ApiSessionError, SessionStatus,
};

use super::super::{question_answer_message, question_restore_error, validate_question_answers};
use super::support::{request_question_answers, request_session, request_session_creation};

#[tokio::test]
async fn runtime_backend_restores_claimed_questions_when_resume_is_rejected() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let project_id = app.active_project_id();
    let session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Draft,
            project_id,
        },
    )
    .await
    .expect("draft session should be created");
    app.services
        .db()
        .sessions()
        .update_session_questions(
            &session_id,
            r#"[{"text":"Current question?","options":[]}]"#,
        )
        .await
        .expect("current questions should persist");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, SessionStatus::Merged);

    // Act
    let answer_error = request_question_answers(
        &mut app,
        session_id.clone(),
        AnswerQuestionsRequest {
            answers: vec![QuestionAnswer {
                answer: "Current answer".to_string(),
                question: "Current question?".to_string(),
            }],
        },
    )
    .await
    .expect_err("a read-only session should reject question answers");
    let session = request_session(&mut app, session_id.clone())
        .await
        .expect("session should load")
        .expect("session should exist");

    // Assert
    assert_eq!(
        answer_error,
        ApiSessionError::Operation(format!(
            "Session `{session_id}` cannot accept question answers in status `Merged`"
        ))
    );
    assert_eq!(session.questions, [QuestionItem::new("Current question?")]);
}

#[test]
fn structured_question_answers_require_current_non_empty_pairs() {
    // Arrange
    let questions = vec![
        QuestionItem::new("Which target?"),
        QuestionItem::new("Run tests?"),
    ];
    let valid_answers = vec![
        QuestionAnswer {
            answer: "main".to_string(),
            question: "Which target?".to_string(),
        },
        QuestionAnswer {
            answer: "yes".to_string(),
            question: "Run tests?".to_string(),
        },
    ];
    let mut stale_answers = valid_answers.clone();
    stale_answers[1].question = "Different question".to_string();
    let mut empty_answers = valid_answers.clone();
    empty_answers[0].answer = " ".to_string();

    // Act
    let valid_result = validate_question_answers(&questions, &valid_answers);
    let no_questions_error =
        validate_question_answers(&[], &[]).expect_err("empty question set should fail");
    let missing_error = validate_question_answers(&questions, &valid_answers[..1])
        .expect_err("missing answer should fail");
    let stale_error = validate_question_answers(&questions, &stale_answers)
        .expect_err("stale answer should fail");
    let empty_error = validate_question_answers(&questions, &empty_answers)
        .expect_err("empty answer should fail");
    let message = question_answer_message(&valid_answers);

    // Assert
    assert_eq!(valid_result, Ok(()));
    assert_eq!(
        no_questions_error,
        ApiSessionError::Operation("Session has no questions to answer".to_string())
    );
    assert_eq!(
        missing_error,
        ApiSessionError::Operation("Expected 2 question answers, received 1".to_string())
    );
    assert_eq!(
        stale_error,
        ApiSessionError::Operation("Question answer 2 is stale".to_string())
    );
    assert_eq!(
        empty_error,
        ApiSessionError::Operation("Question answer 1 is empty".to_string())
    );
    assert_eq!(
        message,
        "Clarifications:\n1. Q: Which target?\n   A: main\n2. Q: Run tests?\n   A: yes"
    );
}

#[test]
fn question_restore_error_preserves_both_failures() {
    // Arrange
    let send_error =
        ApiSessionError::Operation("Session cannot accept question answers".to_string());

    // Act
    let error = question_restore_error(&send_error, &"database unavailable");

    // Assert
    assert_eq!(
        error,
        ApiSessionError::Operation(
            "Session cannot accept question answers; failed to restore session questions: \
             database unavailable"
                .to_string()
        )
    );
}
