use std::sync::Arc;

use ag_agent::{AgentKind, AgentModel, AgentRequestKind, AgentSelection};
use ag_protocol::QuestionItem;
use ag_session::{
    CreateSessionMode, CreateSessionRequest, SessionError as ApiSessionError, SessionStatus,
};

use super::support::{
    clarification_answer_count, current_question_answer, question_transition_app_server,
    request_message, request_question_answers, request_session, request_session_creation,
};

#[tokio::test]
async fn runtime_backend_queues_one_question_resume_behind_turn_entering_question_state() {
    // Arrange
    let (turn_started_tx, mut turn_started_rx) = tokio::sync::mpsc::unbounded_channel();
    let first_turn_release = Arc::new(tokio::sync::Notify::new());
    let app_server =
        question_transition_app_server(Arc::clone(&first_turn_release), turn_started_tx);
    let clients = crate::test_support::test_app_clients()
        .with_app_server_client_override(Arc::new(app_server));
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app_with_clients(clients).await;
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
    app.set_session_model(
        &session_id,
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
    )
    .await
    .expect("session model should update");
    request_message(&mut app, session_id.clone(), "initial prompt")
        .await
        .expect("initial turn should start");
    crate::test_support::finish_session_creation_tasks(&mut app).await;
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), turn_started_rx.recv())
            .await
            .expect("initial turn should start")
            .expect("initial turn kind should be available"),
        AgentRequestKind::SessionStart
    );
    app.services
        .db()
        .sessions()
        .update_session_questions(
            &session_id,
            r#"[{"text":"Current question?","options":[]}]"#,
        )
        .await
        .expect("current questions should persist");
    crate::test_support::set_session_status_for_test(
        &mut app,
        &session_id,
        SessionStatus::InProgress,
    );
    let cached_session = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
        .expect("session should be loaded");
    cached_session.questions = vec![QuestionItem::new("Stale question?")];
    assert_eq!(cached_session.status, SessionStatus::InProgress);

    // Act
    let answer_result = request_question_answers(
        &mut app,
        session_id.clone(),
        current_question_answer("Current answer"),
    )
    .await;
    let duplicate_answer_error = request_question_answers(
        &mut app,
        session_id.clone(),
        current_question_answer("Duplicate answer"),
    )
    .await
    .expect_err("the persisted question set should be consumed once");
    let queued_messages_before_transition = app
        .sessions
        .session_for_id(&session_id)
        .expect("session should stay loaded")
        .queued_messages
        .clone();
    first_turn_release.notify_one();
    let resumed_turn_kind =
        tokio::time::timeout(std::time::Duration::from_secs(1), turn_started_rx.recv())
            .await
            .expect("question answer should resume")
            .expect("resumed turn kind should be available");
    let session = request_session(&mut app, session_id)
        .await
        .expect("session should load")
        .expect("session should exist");

    // Assert
    assert_eq!(answer_result, Ok(()));
    assert_eq!(
        duplicate_answer_error,
        ApiSessionError::Operation("Session has no questions to answer".to_string())
    );
    assert_eq!(resumed_turn_kind, AgentRequestKind::SessionResume);
    assert_eq!(queued_messages_before_transition, []);
    assert_eq!(session.questions, [] as [ag_protocol::QuestionItem; 0]);
    assert_eq!(session.queued_messages, [] as [std::string::String; 0]);
    assert_eq!(clarification_answer_count(&session), 1);
}

#[tokio::test]
async fn runtime_backend_does_not_persist_question_answer_when_worker_enqueue_fails() {
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
    crate::test_support::set_session_status_for_test(
        &mut app,
        &session_id,
        SessionStatus::InProgress,
    );

    // Act
    let answer_error = request_question_answers(
        &mut app,
        session_id.clone(),
        current_question_answer("Current answer"),
    )
    .await
    .expect_err("missing active worker should reject question answers");
    let session = request_session(&mut app, session_id.clone())
        .await
        .expect("session should load")
        .expect("session should exist");

    // Assert
    assert_eq!(
        answer_error,
        ApiSessionError::Operation(format!(
            "Session `{session_id}` cannot accept question answers in status `InProgress`"
        ))
    );
    assert_eq!(session.questions, [QuestionItem::new("Current question?")]);
    assert_eq!(session.messages, [] as [ag_session::SessionMessage; 0]);
}
