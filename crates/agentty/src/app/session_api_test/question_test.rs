use std::sync::Arc;

use ag_agent::{AgentKind, AgentModel, AgentRequestKind, AgentSelection};
use ag_protocol::QuestionItem;
use ag_session::{
    AnswerQuestionsRequest, CreateSessionMode, CreateSessionRequest, QuestionAnswer,
    SessionError as ApiSessionError, SessionStatus,
};

use super::super::{question_answer_message, question_restore_error, validate_question_answers};
use super::support::{
    current_question_answer, question_transition_app_server, request_question_answers,
    request_session, request_session_creation, seed_active_orchestration_child,
};
use crate::domain::orchestration::OrchestrationTaskStatus;

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

#[tokio::test]
async fn orchestration_question_target_requires_an_available_relayed_child() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let fixture = seed_active_orchestration_child(&mut app, true).await;

    // Act
    let missing_relay = app
        .orchestration_question_target(&fixture.controller)
        .await
        .expect("an orchestration without a relay should remain available");
    app.services
        .db()
        .orchestrations()
        .update_orchestration_task_status(
            fixture.task,
            &OrchestrationTaskStatus::WaitingForInput.to_string(),
            None,
        )
        .await
        .expect("managed worker task should wait for input");
    let surfaced = app
        .services
        .db()
        .orchestrations()
        .surface_orchestration_questions(
            fixture.orchestration,
            fixture.task,
            r#"[{"text":"Current question?","options":[]}]"#,
        )
        .await
        .expect("managed worker questions should surface");
    let detached = app
        .services
        .db()
        .orchestrations()
        .detach_orchestration_child(&fixture.child)
        .await
        .expect("managed worker should detach");
    let unavailable_relay_error = app
        .orchestration_question_target(&fixture.controller)
        .await
        .expect_err("a relay without its child should fail explicitly");

    // Assert
    assert_eq!(missing_relay, None);
    assert!(surfaced);
    assert!(detached);
    assert_eq!(
        unavailable_relay_error,
        ApiSessionError::Operation(format!(
            "Orchestration question relay references unavailable task `{}`",
            fixture.task
        ))
    );
}

#[tokio::test]
async fn controller_question_answers_proxy_to_the_managed_worker() {
    // Arrange
    let (turn_started_tx, mut turn_started_rx) = tokio::sync::mpsc::unbounded_channel();
    let first_turn_release = Arc::new(tokio::sync::Notify::new());
    let app_server =
        question_transition_app_server(Arc::clone(&first_turn_release), turn_started_tx);
    let clients = crate::test_support::test_app_clients()
        .with_app_server_client_override(Arc::new(app_server));
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app_with_clients(clients).await;
    let fixture = seed_active_orchestration_child(&mut app, true).await;
    app.set_session_model(
        &fixture.child,
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
    )
    .await
    .expect("managed worker model should update");
    let coordinator_service = app.coordinator_session_service();
    let child_session_id = fixture.child.clone();
    app.drive_session_request(async move {
        coordinator_service
            .send_message(&child_session_id, "initial prompt".to_string())
            .await
    })
    .await
    .expect("coordinator should start the managed worker");
    tokio::time::timeout(std::time::Duration::from_secs(1), turn_started_rx.recv())
        .await
        .expect("managed worker turn should start")
        .expect("managed worker turn signal should be available");
    let questions_json = r#"[{"text":"Current question?","options":[]}]"#;
    app.services
        .db()
        .sessions()
        .update_session_questions(&fixture.child, questions_json)
        .await
        .expect("managed worker questions should persist");
    crate::test_support::set_session_status_for_test(
        &mut app,
        &fixture.child,
        SessionStatus::InProgress,
    );
    let tasks = app
        .services
        .db()
        .orchestrations()
        .load_orchestration_tasks(fixture.orchestration)
        .await
        .expect("campaign tasks should load");
    app.services
        .db()
        .orchestrations()
        .update_orchestration_task_status(
            tasks[0].id,
            &OrchestrationTaskStatus::WaitingForInput.to_string(),
            None,
        )
        .await
        .expect("managed worker task should wait for input");
    app.services
        .db()
        .orchestrations()
        .surface_orchestration_questions(fixture.orchestration, tasks[0].id, questions_json)
        .await
        .expect("managed worker questions should surface");

    // Act
    let answer_result = request_question_answers(
        &mut app,
        fixture.controller.clone(),
        current_question_answer("Current answer"),
    )
    .await;
    assert_eq!(answer_result, Ok(()));
    first_turn_release.notify_one();
    let resumed_turn_kind =
        tokio::time::timeout(std::time::Duration::from_secs(1), turn_started_rx.recv())
            .await
            .expect("proxied answer should resume the worker")
            .expect("resumed worker turn should be available");
    let controller = request_session(&mut app, fixture.controller)
        .await
        .expect("controller should load")
        .expect("controller should exist");

    // Assert
    assert_eq!(resumed_turn_kind, AgentRequestKind::SessionResume);
    assert_eq!(controller.questions, [] as [ag_protocol::QuestionItem; 0]);
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
