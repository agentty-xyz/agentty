use std::sync::Arc;
use std::time::Duration;

use ag_contracts::MockAgentChannel;
use ag_session::{
    AnswerQuestionsRequest, CreateSessionMode, CreateSessionRequest, DurableMessageRequest,
    DurableMessageVisibility, QuestionAnswer, SessionError as ApiSessionError, SessionId,
    SessionMessageKind, SessionStatus,
};
use tokio::sync::Notify;

use super::super::{api_error_from_app, api_error_from_session};
use super::support::{
    request_cancellation, request_durable_message, request_merge, request_message,
    request_question_answers, request_review_request, request_session, request_session_creation,
};
use crate::app::test_support::TestSessionRunFactory;
use crate::app::{AppError, SessionError};
use crate::domain::session::Status;
use crate::presentation::app_mode::{DiffCommentTarget, DiffLineComments};

#[tokio::test]
async fn runtime_backend_durable_turn_clears_diff_comments() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let project_id = app.active_project_id();
    let session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Regular,
            project_id,
        },
    )
    .await
    .expect("session should be created");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, SessionStatus::Review);
    let mut line_comments = DiffLineComments::default();
    line_comments.start_editing_target(DiffCommentTarget::file("src/main.rs"));
    line_comments
        .editing_input_mut()
        .expect("controller diff comment should be editable")
        .insert_text("Review the controller change");
    line_comments.finish_editing();
    app.save_diff_comment_progress(session_id.clone(), line_comments);
    let started = Arc::new(Notify::new());
    let started_for_agent = Arc::clone(&started);
    let mut channel = MockAgentChannel::new();
    channel
        .expect_run_turn()
        .once()
        .returning(move |_, request, _| {
            assert_eq!(request.prompt.text, "Summarize the worker results");
            started_for_agent.notify_one();

            Box::pin(std::future::pending())
        });
    TestSessionRunFactory::install(&mut app.services).register(&session_id, Arc::new(channel));

    // Act
    request_durable_message(
        &mut app,
        session_id.clone(),
        DurableMessageRequest {
            message: "Summarize the worker results".to_string(),
            operation_id: "background-summary-42".to_string(),
            visibility: DurableMessageVisibility::Hidden,
        },
    )
    .await
    .expect("durable turn should be accepted");
    tokio::time::timeout(Duration::from_secs(1), async {
        started.notified().await;
        while app.diff_comment_progress.contains_key(&session_id) {
            app.process_pending_app_events().await;
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("durable turn should clear saved diff comments when it starts");
    let messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(&session_id)
        .await
        .expect("durable transcript should load");

    // Assert
    assert!(!app.diff_comment_progress.contains_key(&session_id));
    assert!(messages.iter().all(|message| {
        message.kind != SessionMessageKind::UserPrompt.to_string()
            || message.content != "Summarize the worker results"
    }));
}

#[tokio::test]
async fn runtime_backend_rejects_invalid_permission_mode_retrieval_and_inheritance() {
    // Arrange
    let (mut app, _temp_dir, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let project_id = app.active_project_id();
    let source_session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Draft,
            project_id,
        },
    )
    .await
    .expect("source session should be created");
    sqlx::query("UPDATE session SET permission_mode = 'invalid' WHERE id = ?")
        .bind(source_session_id.as_str())
        .execute(&pool)
        .await
        .expect("source permission mode should be corrupted");

    // Act
    let retrieval_error = request_session(&mut app, source_session_id.clone())
        .await
        .expect_err("invalid permission mode retrieval should fail");
    let inheritance_error = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: Some(source_session_id),
            mode: CreateSessionMode::Draft,
            project_id,
        },
    )
    .await
    .expect_err("invalid permission mode inheritance should fail");

    // Assert
    for error in [retrieval_error, inheritance_error] {
        assert!(matches!(
            error,
            ApiSessionError::InvalidData(message)
                if message.contains("Unknown permission mode: invalid")
        ));
    }
}

#[tokio::test]
async fn runtime_backend_preserves_workflow_validation_errors() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let project_id = app.active_project_id();

    // Act
    let project_error = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Draft,
            project_id: project_id.saturating_add(1),
        },
    )
    .await
    .expect_err("missing project should fail");
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

    let empty_message_error = request_message(&mut app, session_id.clone(), "  ")
        .await
        .expect_err("empty message should fail");
    let missing_message_error = request_message(&mut app, SessionId::from("missing"), "continue")
        .await
        .expect_err("missing session should fail");
    let stale_answers_error = request_question_answers(
        &mut app,
        session_id.clone(),
        AnswerQuestionsRequest {
            answers: vec![QuestionAnswer {
                answer: "main".to_string(),
                question: "Which target?".to_string(),
            }],
        },
    )
    .await
    .expect_err("unexpected answer set should fail");
    let cancel_error = request_cancellation(&mut app, SessionId::from("missing"))
        .await
        .expect_err("missing cancellation should fail");
    app.sessions
        .session_at_mut(0)
        .expect("draft session should be loaded")
        .status = SessionStatus::InProgress;
    if let Some(handles) = app.sessions.session_handles().get(&session_id)
        && let Ok(mut status) = handles.status.lock()
    {
        *status = SessionStatus::InProgress;
    }
    request_message(&mut app, session_id.clone(), "queued follow-up")
        .await
        .expect("running session should queue the message");
    let queued_session = request_session(&mut app, session_id.clone())
        .await
        .expect("session should load")
        .expect("session should exist");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, SessionStatus::Done);
    let terminal_message_error = request_message(&mut app, session_id.clone(), "too late")
        .await
        .expect_err("terminal session should reject messages");
    let merge_error = request_merge(&mut app, session_id.clone())
        .await
        .expect_err("draft session should not merge");
    let missing_review_error = request_review_request(&mut app, SessionId::from("missing-review"))
        .await
        .expect_err("missing session should not publish");
    let review_error = request_review_request(&mut app, session_id)
        .await
        .expect_err("draft session should not publish");

    // Assert
    assert!(matches!(project_error, ApiSessionError::Operation(_)));
    assert!(matches!(empty_message_error, ApiSessionError::Operation(_)));
    assert_eq!(missing_message_error, ApiSessionError::NotFound);
    assert!(matches!(stale_answers_error, ApiSessionError::Operation(_)));
    assert_eq!(cancel_error, ApiSessionError::NotFound);
    assert_eq!(queued_session.queued_messages, ["queued follow-up"]);
    assert!(matches!(
        terminal_message_error,
        ApiSessionError::Operation(_)
    ));
    assert!(matches!(merge_error, ApiSessionError::Operation(_)));
    assert_eq!(missing_review_error, ApiSessionError::NotFound);
    assert!(matches!(review_error, ApiSessionError::Operation(_)));
}

#[tokio::test]
async fn runtime_backend_reports_session_read_failures() {
    // Arrange
    let (mut session_query_app, _session_temp_dir, session_pool) =
        crate::test_support::new_git_test_app_with_pool().await;
    let (mut message_query_app, _message_temp_dir, message_pool) =
        crate::test_support::new_git_test_app_with_pool().await;
    let message_project_id = message_query_app.active_project_id();
    let message_session_id = request_session_creation(
        &mut message_query_app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Regular,
            project_id: message_project_id,
        },
    )
    .await
    .expect("session should be created");
    sqlx::query("DROP TABLE session")
        .execute(&session_pool)
        .await
        .expect("session table should be dropped");
    sqlx::query("DROP TABLE session_message")
        .execute(&message_pool)
        .await
        .expect("message table should be dropped");

    // Act
    let session_query_error = request_session(&mut session_query_app, SessionId::from("missing"))
        .await
        .expect_err("session query should fail");
    let message_query_error = request_session(&mut message_query_app, message_session_id)
        .await
        .expect_err("message query should fail");

    // Assert
    assert!(matches!(session_query_error, ApiSessionError::Operation(_)));
    assert!(matches!(message_query_error, ApiSessionError::Operation(_)));
}

#[test]
fn api_error_translation_preserves_not_found() {
    // Arrange / Act
    let app_error = api_error_from_app(AppError::Session(SessionError::NotFound));
    let session_error = api_error_from_session(SessionError::NotFound);
    let workflow_error = api_error_from_app(AppError::Workflow("workflow failed".to_string()));

    // Assert
    assert_eq!(app_error, ApiSessionError::NotFound);
    assert_eq!(session_error, ApiSessionError::NotFound);
    assert_eq!(
        workflow_error,
        ApiSessionError::Operation("workflow failed".to_string())
    );
}

#[tokio::test]
async fn durable_messages_require_content_and_operation_id() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_test_app().await;
    let session_id = SessionId::from("missing");
    let (mut busy_app, _busy_temp_dir) = crate::test_support::new_test_app().await;
    busy_app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .status(Status::InProgress)
            .build(),
    );
    let (mut unbound_app, _unbound_temp_dir) = crate::test_support::new_test_app().await;
    unbound_app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .status(Status::Review)
            .build(),
    );
    let existing_session_id = SessionId::from("session-id");

    // Act
    let empty_message_error = app
        .submit_api_durable_message(
            &session_id,
            DurableMessageRequest {
                message: " ".to_string(),
                operation_id: "rollup-1".to_string(),
                visibility: DurableMessageVisibility::Hidden,
            },
        )
        .await
        .expect_err("empty durable message should fail");
    let empty_operation_error = app
        .submit_api_durable_message(
            &session_id,
            DurableMessageRequest {
                message: "Roll up".to_string(),
                operation_id: " ".to_string(),
                visibility: DurableMessageVisibility::Hidden,
            },
        )
        .await
        .expect_err("empty durable operation id should fail");
    let busy_error = busy_app
        .submit_api_durable_message(
            &existing_session_id,
            DurableMessageRequest {
                message: "Roll up".to_string(),
                operation_id: "rollup-busy".to_string(),
                visibility: DurableMessageVisibility::Hidden,
            },
        )
        .await
        .expect_err("busy durable should reject a roll-up");
    let enqueue_error = unbound_app
        .submit_api_durable_message(
            &existing_session_id,
            DurableMessageRequest {
                message: "Roll up".to_string(),
                operation_id: "rollup-unbound".to_string(),
                visibility: DurableMessageVisibility::Hidden,
            },
        )
        .await
        .expect_err("durable without a worker should reject a roll-up");

    // Assert
    assert_eq!(
        empty_message_error,
        ApiSessionError::Operation("Cannot submit an empty durable message".to_string())
    );
    assert_eq!(
        empty_operation_error,
        ApiSessionError::Operation(
            "Cannot submit a durable message without an operation id".to_string()
        )
    );
    assert_eq!(
        busy_error,
        ApiSessionError::Operation(
            "Session `session-id` cannot accept a durable message in status `InProgress`"
                .to_string()
        )
    );
    assert_eq!(
        enqueue_error,
        ApiSessionError::Operation(
            "Session `session-id` could not enqueue the durable message".to_string()
        )
    );
}
