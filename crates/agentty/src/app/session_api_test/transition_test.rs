use std::time::Duration;

use ag_session::{
    AnswerQuestionsRequest, CoordinatorMessageRequest, CoordinatorMessageVisibility,
    CreateSessionMode, CreateSessionRequest, QuestionAnswer, SessionError as ApiSessionError,
    SessionId, SessionMessageKind, SessionRole, SessionStatus,
};
use tokio::sync::oneshot;

use super::super::{api_error_from_app, api_error_from_session};
use super::support::{
    request_cancellation, request_coordinator_message, request_merge, request_message,
    request_question_answers, request_review_request, request_session, request_session_creation,
    seed_active_orchestration_child,
};
use crate::app::{AppError, SessionError, SessionRuntimeAccess};
use crate::domain::orchestration::{OrchestrationStatus, OrchestrationTaskStatus};
use crate::domain::session::Status;
use crate::presentation::app_mode::{DiffCommentTarget, DiffLineComments};

#[tokio::test]
async fn runtime_backend_coordinator_turn_clears_diff_comments() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let project_id = app.active_project_id();
    let session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Orchestrator,
            project_id,
        },
    )
    .await
    .expect("orchestrator should be created");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, SessionStatus::Review);
    let mut line_comments = DiffLineComments::default();
    line_comments.start_editing_target(DiffCommentTarget::file("src/main.rs"));
    line_comments
        .editing_input_mut()
        .expect("controller diff comment should be editable")
        .insert_text("Review the controller change");
    line_comments.finish_editing();
    app.save_diff_comment_progress(session_id.clone(), line_comments);

    // Act
    let review_request_error = request_review_request(&mut app, session_id.clone())
        .await
        .expect_err("orchestrator review request should fail");
    request_coordinator_message(
        &mut app,
        session_id.clone(),
        CoordinatorMessageRequest {
            message: "Summarize the worker results".to_string(),
            operation_id: "orchestration-rollup-42".to_string(),
            visibility: CoordinatorMessageVisibility::Hidden,
        },
    )
    .await
    .expect("coordinator turn should be accepted");
    tokio::time::timeout(Duration::from_secs(1), async {
        while app.diff_comment_progress.contains_key(&session_id) {
            app.process_pending_app_events().await;
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("coordinator turn should clear saved diff comments when it starts");
    let messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(&session_id)
        .await
        .expect("coordinator transcript should load");

    // Assert
    assert_eq!(
        review_request_error,
        ApiSessionError::Operation(
            "Orchestrator sessions cannot publish review requests".to_string()
        )
    );
    assert!(!app.diff_comment_progress.contains_key(&session_id));
    assert!(messages.iter().all(|message| {
        message.kind != SessionMessageKind::UserPrompt.to_string()
            || message.content != "Summarize the worker results"
    }));
}

#[tokio::test]
async fn runtime_backend_cascade_cancels_orchestrator_children_and_tasks() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let fixture = seed_active_orchestration_child(&mut app, true).await;
    app.sessions.update_orchestration_progress(
        &fixture.controller,
        Some("Working... child: running".to_string()),
    );

    // Act
    request_cancellation(&mut app, fixture.controller.clone())
        .await
        .expect("orchestrator cancellation should cascade");
    let controller = request_session(&mut app, fixture.controller.clone())
        .await
        .expect("controller should load")
        .expect("controller should exist");
    let child = request_session(&mut app, fixture.child)
        .await
        .expect("child should load")
        .expect("child should exist");
    let orchestration = app
        .services
        .db()
        .orchestrations()
        .load_orchestration_for_controller(&fixture.controller)
        .await
        .expect("orchestration should load")
        .expect("orchestration should exist");
    let tasks = app
        .services
        .db()
        .orchestrations()
        .load_orchestration_tasks(fixture.orchestration)
        .await
        .expect("orchestration tasks should load");

    // Assert
    assert_eq!(controller.status, SessionStatus::Canceled);
    assert_eq!(child.status, SessionStatus::Canceled);
    assert_eq!(
        orchestration.status,
        OrchestrationStatus::Canceled.to_string()
    );
    assert_eq!(
        tasks[0].status,
        OrchestrationTaskStatus::Canceled.to_string()
    );
    assert!(
        app.sessions
            .sessions()
            .iter()
            .find(|session| session.id == fixture.controller)
            .and_then(|session| {
                session
                    .transient_messages
                    .get(crate::domain::transient_message::TransientMessageSlot::Orchestration)
            })
            .is_none()
    );
}

#[tokio::test]
async fn runtime_backend_preserves_orchestration_when_child_cancellation_fails() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let fixture = seed_active_orchestration_child(&mut app, true).await;
    crate::test_support::set_session_status_for_test(&mut app, &fixture.child, SessionStatus::Done);
    let controller_before = request_session(&mut app, fixture.controller.clone())
        .await
        .expect("controller should load before cancellation")
        .expect("controller should exist before cancellation");

    // Act
    let cancel_error = request_cancellation(&mut app, fixture.controller.clone())
        .await
        .expect_err("terminal child should prevent false cascade success");
    let controller = request_session(&mut app, fixture.controller.clone())
        .await
        .expect("controller should load")
        .expect("controller should exist");
    let orchestration = app
        .services
        .db()
        .orchestrations()
        .load_orchestration_for_controller(&fixture.controller)
        .await
        .expect("orchestration should load")
        .expect("orchestration should exist");
    let tasks = app
        .services
        .db()
        .orchestrations()
        .load_orchestration_tasks(fixture.orchestration)
        .await
        .expect("orchestration tasks should load");

    // Assert
    assert!(
        cancel_error
            .to_string()
            .contains("not cancelable in its current state")
    );
    assert_eq!(controller.status, controller_before.status);
    assert_eq!(
        orchestration.status,
        OrchestrationStatus::Canceling.to_string()
    );
    assert_eq!(
        tasks[0].status,
        OrchestrationTaskStatus::Running.to_string()
    );
}

#[tokio::test]
async fn runtime_backend_cancels_reverse_linked_orchestration_child() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let fixture = seed_active_orchestration_child(&mut app, false).await;

    // Act
    request_cancellation(&mut app, fixture.controller.clone())
        .await
        .expect("reverse-linked child cancellation should cascade");
    let child = request_session(&mut app, fixture.child)
        .await
        .expect("child should load")
        .expect("child should exist");
    let tasks = app
        .services
        .db()
        .orchestrations()
        .load_orchestration_tasks(fixture.orchestration)
        .await
        .expect("orchestration tasks should load");

    // Assert
    assert_eq!(child.status, SessionStatus::Canceled);
    assert_eq!(
        tasks[0].status,
        OrchestrationTaskStatus::Canceled.to_string()
    );
    assert!(tasks[0].child_session_id.is_none());
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

#[tokio::test]
async fn user_capability_rejects_every_managed_child_mutation() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = SessionId::from("session-id");
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .role(SessionRole::OrchestrationWorker)
            .status(Status::Review)
            .build(),
    );
    let (publish_tx, publish_rx) = oneshot::channel();

    // Act
    let send_error = app
        .send_api_message(
            &session_id,
            "Direct edit".to_string(),
            SessionRuntimeAccess::User,
        )
        .await
        .expect_err("managed child message should fail");
    let answer_error = app
        .answer_api_questions(
            &session_id,
            AnswerQuestionsRequest {
                answers: Vec::new(),
            },
            SessionRuntimeAccess::User,
        )
        .await
        .expect_err("managed child answers should fail");
    let cancel_error = app
        .cancel_api_session(&session_id, SessionRuntimeAccess::User)
        .await
        .expect_err("managed child cancellation should fail");
    let merge_error = app
        .merge_api_session(&session_id, SessionRuntimeAccess::User)
        .await
        .expect_err("managed child merge should fail");
    app.start_api_review_request_publish(
        session_id.clone(),
        SessionRuntimeAccess::User,
        publish_tx,
    )
    .await;
    let publish_error = publish_rx
        .await
        .expect("publish result should be returned")
        .expect_err("managed child publish should fail");

    // Assert
    for error in [
        send_error,
        answer_error,
        cancel_error,
        merge_error,
        publish_error,
    ] {
        assert!(matches!(
            error,
            ApiSessionError::Operation(message)
                if message.contains("managed by an orchestration campaign")
        ));
    }
}

#[tokio::test]
async fn cancel_api_orchestration_ignores_missing_orchestration() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = SessionId::from("missing-orchestration");

    // Act
    let result = app.cancel_api_orchestration(&session_id).await;

    // Assert
    assert_eq!(result, Ok(()));
}

#[tokio::test]
async fn cancel_api_orchestration_ignores_settled_orchestration() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let fixture = seed_active_orchestration_child(&mut app, true).await;
    app.services
        .db()
        .orchestrations()
        .update_orchestration_status(
            fixture.orchestration,
            &OrchestrationStatus::Done.to_string(),
        )
        .await
        .expect("orchestration should settle");

    // Act
    let result = app.cancel_api_orchestration(&fixture.controller).await;
    let orchestration = app
        .services
        .db()
        .orchestrations()
        .load_orchestration_for_controller(&fixture.controller)
        .await
        .expect("orchestration should load")
        .expect("orchestration should exist");

    // Assert
    assert_eq!(result, Ok(()));
    assert_eq!(orchestration.status, OrchestrationStatus::Done.to_string());
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
async fn coordinator_capability_cancels_managed_workers_and_regular_sessions_still_cancel() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let fixture = seed_active_orchestration_child(&mut app, true).await;
    let coordinator_service = app.coordinator_session_service();
    let managed_child = fixture.child.clone();
    let project_id = app.active_project_id();
    let regular_session = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Regular,
            project_id,
        },
    )
    .await
    .expect("regular session should be created");
    crate::test_support::set_session_status_for_test(
        &mut app,
        &regular_session,
        SessionStatus::Review,
    );

    // Act
    let managed_result = app
        .drive_session_request(
            async move { coordinator_service.cancel_session(&managed_child).await },
        )
        .await;
    let regular_result = request_cancellation(&mut app, regular_session.clone()).await;
    let managed = request_session(&mut app, fixture.child)
        .await
        .expect("managed worker should load")
        .expect("managed worker should exist");
    let regular = request_session(&mut app, regular_session)
        .await
        .expect("regular session should load")
        .expect("regular session should exist");

    // Assert
    assert_eq!(managed_result, Ok(()));
    assert_eq!(regular_result, Ok(()));
    assert_eq!(managed.status, SessionStatus::Canceled);
    assert_eq!(regular.status, SessionStatus::Canceled);
}

#[tokio::test]
async fn coordinator_messages_require_content_and_operation_id() {
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
        .submit_api_coordinator_message(
            &session_id,
            CoordinatorMessageRequest {
                message: " ".to_string(),
                operation_id: "rollup-1".to_string(),
                visibility: CoordinatorMessageVisibility::Hidden,
            },
        )
        .await
        .expect_err("empty coordinator message should fail");
    let empty_operation_error = app
        .submit_api_coordinator_message(
            &session_id,
            CoordinatorMessageRequest {
                message: "Roll up".to_string(),
                operation_id: " ".to_string(),
                visibility: CoordinatorMessageVisibility::Hidden,
            },
        )
        .await
        .expect_err("empty coordinator operation id should fail");
    let busy_error = busy_app
        .submit_api_coordinator_message(
            &existing_session_id,
            CoordinatorMessageRequest {
                message: "Roll up".to_string(),
                operation_id: "rollup-busy".to_string(),
                visibility: CoordinatorMessageVisibility::Hidden,
            },
        )
        .await
        .expect_err("busy coordinator should reject a roll-up");
    let enqueue_error = unbound_app
        .submit_api_coordinator_message(
            &existing_session_id,
            CoordinatorMessageRequest {
                message: "Roll up".to_string(),
                operation_id: "rollup-unbound".to_string(),
                visibility: CoordinatorMessageVisibility::Hidden,
            },
        )
        .await
        .expect_err("coordinator without a worker should reject a roll-up");

    // Assert
    assert_eq!(
        empty_message_error,
        ApiSessionError::Operation("Cannot submit an empty coordinator message".to_string())
    );
    assert_eq!(
        empty_operation_error,
        ApiSessionError::Operation(
            "Cannot submit a coordinator message without an operation id".to_string()
        )
    );
    assert_eq!(
        busy_error,
        ApiSessionError::Operation(
            "Session `session-id` cannot accept a coordinator message in status `InProgress`"
                .to_string()
        )
    );
    assert_eq!(
        enqueue_error,
        ApiSessionError::Operation(
            "Session `session-id` could not enqueue the coordinator message".to_string()
        )
    );
}
