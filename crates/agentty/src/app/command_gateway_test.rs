use std::sync::{Arc, Mutex};

use ag_session::{
    DurableMessageRequest, DurableMessageVisibility, SessionError, SessionId, SessionStatus,
};
use tokio::sync::oneshot;

use super::{
    CommandDelivery, CommandInitiator, ReviewRequestCommand, SessionCommand, SessionMutation,
};
use crate::app::SessionRuntimeCommand;
use crate::app::message_admission::MessageIntent;
use crate::app::prompt_intent::{PromptSessionMode, PromptSubmission};
use crate::domain::session::PublishBranchAction;
use crate::domain::transient_message::{TransientMessageBody, TransientMessageSlot};
use crate::domain::turn_prompt::{TurnPrompt, TurnPromptAttachment, TurnPromptTextSource};
use crate::presentation::app_mode::{AppMode, ConfirmationViewMode};
use crate::test_support::{new_git_test_app, new_test_app, set_session_status_for_test};

/// Captures admission metadata through tracing's pre-initialization log
/// fallback.
struct AdmissionLogger(Mutex<Vec<String>>);

impl tracing::log::Log for AdmissionLogger {
    fn enabled(&self, metadata: &tracing::log::Metadata<'_>) -> bool {
        metadata.level() == tracing::log::Level::Debug
            && metadata.target().ends_with("command_gateway::admission")
    }

    fn log(&self, record: &tracing::log::Record<'_>) {
        if self.enabled(record.metadata()) {
            self.0
                .lock()
                .expect("admission logs")
                .push(record.args().to_string());
        }
    }

    fn flush(&self) {}
}

#[tokio::test]
async fn admission_logs_identity_and_provenance_without_prompt_content() {
    // Arrange: keep process-global logging independent of other subscribers.
    const CHILD_ENV: &str = "AGENTTY_TEST_ADMISSION_LOG_FALLBACK";
    static LOGGER: AdmissionLogger = AdmissionLogger(Mutex::new(Vec::new()));
    if std::env::var_os(CHILD_ENV).is_none() {
        let mut child = tokio::process::Command::new(
            std::env::current_exe().expect("test binary should exist"),
        );
        child.args([
            "--exact",
            "app::command_gateway::tests::admission_logs_identity_and_provenance_without_prompt_content",
            "--nocapture",
        ]).env(CHILD_ENV, "1");
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            child.env(
                "LLVM_PROFILE_FILE",
                profile
                    .to_string_lossy()
                    .replace(".profraw", "-admission-log-child.profraw"),
            );
        }

        // Act
        let status = child
            .status()
            .await
            .expect("isolated admission logging test");

        // Assert
        assert!(status.success());
        return;
    }
    tracing::log::set_logger(&LOGGER).expect("isolated logger should install");
    tracing::log::set_max_level(tracing::log::LevelFilter::Debug);
    let (mut app, _directory) = new_test_app().await;
    let session_id = SessionId::from("missing-session");
    let operation_id = "durable-operation";
    let private_prompt = "private prompt must not appear in admission logs";
    let (response_tx, response_rx) = oneshot::channel();
    let command = SessionCommand::new(
        CommandInitiator::SessionApi,
        CommandDelivery::Admission {
            request: SessionMutation::SubmitDurableMessage {
                session_id,
                request: DurableMessageRequest {
                    message: private_prompt.to_string(),
                    operation_id: operation_id.to_string(),
                    visibility: DurableMessageVisibility::Visible,
                },
            },
            response_tx: Some(response_tx),
        },
    );
    let command_id = command.id.clone();

    // Act
    app.admit_session_command(command)
        .await
        .expect("API result is returned through its response channel");

    // Assert
    assert_eq!(
        response_rx.await.expect("response"),
        Err(SessionError::NotFound)
    );
    let logs = LOGGER.0.lock().expect("admission logs");
    assert_eq!(logs.len(), 1);
    let message = &logs[0];
    for expected in [
        "admitting session command",
        "kind=\"submit_durable_message\"",
        "session_id=",
        "missing-session",
        "command_id=",
        &command_id,
        "correlation_id=",
        operation_id,
        "initiator=SessionApi",
    ] {
        assert!(message.contains(expected), "missing {expected}: {message}");
    }
    assert_ne!(command_id, operation_id);
    assert!(!message.contains(private_prompt));
}

#[tokio::test]
async fn missing_session_preserves_api_error_and_terminal_popup() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let service = app.session_service();
    let session_id = SessionId::from("missing");
    let restore_view = ConfirmationViewMode {
        scroll_offset: Some(5),
        session_id: session_id.clone(),
    };

    // Act
    let api_result = app
        .drive_session_request(service.create_review_request(&session_id))
        .await;
    app.start_publish_branch_action(
        restore_view,
        &session_id,
        PublishBranchAction::PublishPullRequest,
        None,
    )
    .await;

    // Assert
    assert_eq!(api_result, Err(SessionError::NotFound));
    assert!(matches!(app.mode, AppMode::ViewInfoPopup { .. }));
    assert!(
        app.services
            .db()
            .operations()
            .load_unfinished_session_operations()
            .await
            .expect("operations should load")
            .is_empty()
    );
}

#[tokio::test]
async fn missing_handles_rejects_publication_before_persistence() {
    // Arrange
    let (mut app, _directory) = new_git_test_app().await;
    let session_id = SessionId::from(app.create_session().await.expect("session should exist"));
    app.sessions.session_handles_mut().remove(&session_id);
    let (response_tx, response_rx) = oneshot::channel();
    let command = ReviewRequestCommand::new(session_id, None);

    // Act
    let result = app.admit_review_request(command, Some(response_tx)).await;

    // Assert
    assert_eq!(result, Err(SessionError::NotFound));
    assert_eq!(
        response_rx.await.expect("response"),
        Err(SessionError::NotFound)
    );
}

#[tokio::test]
async fn stale_active_session_keeps_inline_failure_without_creating_work_or_chat() {
    // Arrange
    let (mut app, _directory) = new_git_test_app().await;
    let session_id = SessionId::from(app.create_session().await.expect("session should exist"));
    set_session_status_for_test(&mut app, &session_id, SessionStatus::InProgress);
    let restore_view = ConfirmationViewMode {
        scroll_offset: Some(5),
        session_id: session_id.clone(),
    };
    let messages_before = app
        .services
        .db()
        .sessions()
        .load_session_messages(&session_id)
        .await
        .expect("transcript should load");

    // Act
    app.start_publish_branch_action(
        restore_view,
        &session_id,
        PublishBranchAction::PublishPullRequest,
        None,
    )
    .await;
    let operations = app
        .services
        .db()
        .operations()
        .load_unfinished_session_operations()
        .await
        .expect("operations should load");
    let messages_after = app
        .services
        .db()
        .sessions()
        .load_session_messages(&session_id)
        .await
        .expect("transcript should load");
    let notice = app
        .sessions
        .session_for_id(&session_id)
        .expect("session")
        .transient_messages
        .get(TransientMessageSlot::BranchPublish)
        .expect("inline failure should exist");

    // Assert
    assert!(operations.is_empty());
    assert_eq!(messages_before, messages_after);
    assert!(
        notice
            .body
            .text()
            .contains("active session worker is unavailable")
    );
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(5),
            ..
        }
    ));
}

#[tokio::test]
async fn command_identity_correlates_mailbox_admission_with_persisted_operation() {
    // Arrange
    let (mut app, _directory) = new_git_test_app().await;
    let session_id = SessionId::from(app.create_session().await.expect("session should exist"));
    set_session_status_for_test(&mut app, &session_id, SessionStatus::Done);
    let branch_operation_lock = Arc::clone(
        &app.sessions
            .session_handles_or_err(&session_id)
            .expect("session handles")
            .branch_operation_lock,
    );
    let guard = branch_operation_lock.lock_owned().await;
    let handle = app
        .sessions
        .handle()
        .with_initiator(CommandInitiator::SessionView);
    let _consumer = app.sessions.foreground_consumer();
    let request_session_id = session_id.clone();
    let response =
        tokio::spawn(async move { handle.create_review_request(&request_session_id).await });
    let command = app.sessions.next_command().await;
    let operation_id = match &command {
        SessionRuntimeCommand::Mutation(SessionCommand {
            id,
            correlation_id,
            initiator,
            delivery:
                CommandDelivery::Review {
                    request: publication,
                    ..
                },
            ..
        }) => {
            assert_eq!(*initiator, CommandInitiator::SessionView);
            assert_ne!(id, &publication.operation_id);
            assert_eq!(
                correlation_id.as_deref(),
                Some(publication.operation_id.as_str())
            );
            Some(publication.operation_id.clone())
        }
        _ => None,
    }
    .expect("expected publication command");

    // Act
    app.apply_session_runtime_command(command).await;
    let operations = app
        .services
        .db()
        .operations()
        .load_unfinished_session_operations()
        .await
        .expect("operations should load");
    let response_is_pending = !response.is_finished();
    drop(guard);
    let result = response.await.expect("request task should complete");

    // Assert
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].id, operation_id);
    assert_eq!(operations[0].session_id, session_id.as_str());
    assert_eq!(operations[0].kind, "create_review_request");
    assert!(response_is_pending);
    assert_eq!(
        result,
        Err(SessionError::Operation(
            "Session must be in review to publish the review request.".to_string(),
        ))
    );
}

#[tokio::test]
async fn terminal_and_api_admissions_share_worker_order_without_waiting_for_completion() {
    // Arrange
    let (mut app, _directory) = new_git_test_app().await;
    let session_id = SessionId::from(app.create_session().await.expect("session should exist"));
    set_session_status_for_test(&mut app, &session_id, SessionStatus::Done);
    let branch_operation_lock = Arc::clone(
        &app.sessions
            .session_handles_or_err(&session_id)
            .expect("session handles")
            .branch_operation_lock,
    );
    let guard = branch_operation_lock.lock_owned().await;
    let (response_tx, mut response_rx) = oneshot::channel();
    app.admit_review_request(
        ReviewRequestCommand::new(session_id.clone(), None),
        Some(response_tx),
    )
    .await
    .expect("first command should be admitted");
    set_session_status_for_test(&mut app, &session_id, SessionStatus::InProgress);
    let restore_view = ConfirmationViewMode {
        scroll_offset: Some(5),
        session_id: session_id.clone(),
    };

    // Act
    app.start_publish_branch_action(
        restore_view,
        &session_id,
        PublishBranchAction::PublishPullRequest,
        Some("review/custom".to_string()),
    )
    .await;
    let operations = app
        .services
        .db()
        .operations()
        .load_unfinished_session_operations()
        .await
        .expect("operations should load");
    let queued_body = app
        .sessions
        .session_for_id(&session_id)
        .expect("session")
        .transient_messages
        .get(TransientMessageSlot::BranchPublish)
        .map(|message| &message.body);

    // Assert
    assert_eq!(operations.len(), 2);
    assert_ne!(operations[0].id, operations[1].id);
    assert!(
        matches!(queued_body, Some(TransientMessageBody::Queued(action))
        if action.order == 0 && action.text == "review request — publish after this turn")
    );
    assert!(matches!(
        response_rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(5),
            ..
        }
    ));
    drop(guard);
}

#[test]
fn repeated_durable_submissions_have_distinct_command_ids_and_stable_operation_correlation() {
    // Arrange
    let request = || {
        let (response_tx, _response_rx) = oneshot::channel();
        CommandDelivery::Admission {
            request: SessionMutation::SubmitDurableMessage {
                session_id: SessionId::from("session"),
                request: DurableMessageRequest {
                    message: "continue".to_string(),
                    operation_id: "durable-operation".to_string(),
                    visibility: DurableMessageVisibility::Visible,
                },
            },
            response_tx: Some(response_tx),
        }
    };

    // Act
    let first = SessionCommand::new(CommandInitiator::SessionApi, request());
    let second = SessionCommand::new(CommandInitiator::SessionApi, request());

    // Assert
    assert_ne!(first.id, second.id);
    assert_ne!(first.id, "durable-operation");
    assert_eq!(first.correlation_id.as_deref(), Some("durable-operation"));
    assert_eq!(first.correlation_id, second.correlation_id);
    assert_eq!(first.initiator, CommandInitiator::SessionApi);
}

#[tokio::test]
async fn terminal_and_api_mailbox_requests_retain_their_initiators() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let _consumer = app.sessions.foreground_consumer();
    let services = [
        (app.session_view_service(), CommandInitiator::SessionView),
        (app.session_service(), CommandInitiator::SessionApi),
    ];
    let mut command_ids = Vec::new();

    for (service, initiator) in services {
        // Act
        let response =
            tokio::spawn(async move { service.cancel_session(&SessionId::from("missing")).await });
        let command = app.sessions.next_command().await;
        let envelope = match &command {
            SessionRuntimeCommand::Mutation(envelope) => Some(envelope),
            SessionRuntimeCommand::Get { .. }
            | SessionRuntimeCommand::GetExecutionGraph { .. }
            | SessionRuntimeCommand::RunGraphNode { .. }
            | SessionRuntimeCommand::GetTask { .. }
            | SessionRuntimeCommand::ListTasks { .. }
            | SessionRuntimeCommand::GetTaskChain { .. }
            | SessionRuntimeCommand::ListTaskChains { .. }
            | SessionRuntimeCommand::GetAgentTaskHistory { .. } => None,
        }
        .expect("cancellation must be a mutation");

        // Assert
        assert_eq!(envelope.initiator, initiator);
        assert!(envelope.correlation_id.is_none());
        assert!(matches!(
            envelope.delivery,
            CommandDelivery::Admission {
                request: SessionMutation::Cancel { .. },
                response_tx: Some(_),
            }
        ));
        command_ids.push(envelope.id.clone());
        app.apply_session_runtime_command(command).await;
        assert_eq!(
            response.await.expect("request completed"),
            Err(SessionError::NotFound)
        );
    }
    assert_ne!(command_ids[0], command_ids[1]);
}

#[tokio::test]
async fn gateway_keeps_terminal_draft_prompt_and_attachments_staged() {
    // Arrange
    let (mut app, _directory) = new_git_test_app().await;
    let session_id = SessionId::from(app.create_draft_session().await.expect("draft"));
    let prompt = TurnPrompt {
        text: "Inspect @source [Image #1]".to_string(),
        attachments: vec![TurnPromptAttachment {
            local_image_path: "image.png".into(),
            placeholder: "[Image #1]".to_string(),
        }],
        text_source: TurnPromptTextSource::AgentData,
    };

    // Act
    app.submit_prompt(PromptSubmission {
        session_id: session_id.clone(),
        session_mode: PromptSessionMode::NewDraft,
        prompt: prompt.clone(),
    })
    .await;
    app.process_pending_app_events().await;
    let session = app
        .sessions
        .session_for_id(&session_id)
        .expect("draft remains loaded");

    // Assert
    assert_eq!(session.status, SessionStatus::Draft);
    assert_eq!(session.prompt, prompt.text);
    assert_eq!(session.draft_attachments, prompt.attachments);
    assert!(
        app.services
            .db()
            .operations()
            .load_unfinished_session_operations()
            .await
            .expect("operations")
            .is_empty()
    );
}

#[tokio::test]
async fn staged_start_admission_retains_missing_session_error() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let session_id = SessionId::from("missing");
    let expected = app
        .start_staged_session(&session_id)
        .await
        .expect_err("missing")
        .to_string();

    // Act
    let actual = app
        .admit_staged_start(session_id)
        .await
        .expect_err("missing");

    // Assert
    assert_eq!(actual.to_string(), expected);
}

#[tokio::test]
async fn message_plans_preserve_busy_queue_order_and_terminal_attachments() {
    // Arrange
    let (mut app, _directory) = new_git_test_app().await;
    let service = app.session_service();
    for status in [SessionStatus::InProgress, SessionStatus::Rebasing] {
        let session_id = SessionId::from(app.create_session().await.expect("session"));
        set_session_status_for_test(&mut app, &session_id, status);
        let prompt = TurnPrompt {
            text: "Inspect [Image #1]".to_string(),
            attachments: vec![TurnPromptAttachment {
                local_image_path: "image.png".into(),
                placeholder: "[Image #1]".to_string(),
            }],
            text_source: TurnPromptTextSource::AgentData,
        };

        // Act
        app.admit_session_command(SessionCommand::new(
            CommandInitiator::SessionView,
            CommandDelivery::Admission {
                request: SessionMutation::Prompt {
                    session_id: session_id.clone(),
                    intent: MessageIntent::FollowUp,
                    prompt: prompt.clone(),
                },
                response_tx: None,
            },
        ))
        .await
        .expect("terminal admission");
        let api_result = app
            .drive_session_request(service.send_message(&session_id, "then continue"))
            .await;
        let (response_tx, response_rx) = oneshot::channel();
        drop(response_rx);
        let abandoned_reply = app
            .admit_session_command(SessionCommand::new(
                CommandInitiator::SessionApi,
                CommandDelivery::Admission {
                    request: SessionMutation::SendMessage {
                        message: "accepted even without a listener".to_string(),
                        session_id: session_id.clone(),
                    },
                    response_tx: Some(response_tx),
                },
            ))
            .await;
        let session = app.sessions.session_for_id(&session_id).expect("session");

        // Assert
        assert_eq!(api_result, Ok(()));
        assert_eq!(abandoned_reply, Ok(()));
        assert_eq!(session.status, status);
        assert_eq!(session.queued_messages.len(), 3);
        assert_eq!(session.queued_messages[0].prompt(), &prompt);
        assert_eq!(
            session.queued_messages[1].transcript_text(),
            "then continue"
        );
        assert!(session.queued_messages[0].order() < session.queued_messages[1].order());
        assert_eq!(
            session.queued_messages[2].transcript_text(),
            "accepted even without a listener"
        );
        assert!(session.queued_messages[1].order() < session.queued_messages[2].order());
    }
}

#[tokio::test]
async fn message_planning_preserves_api_validation_precedence_and_reply_rejection() {
    // Arrange
    let (mut app, _directory) = new_git_test_app().await;
    let service = app.session_service();
    let missing_id = SessionId::from("missing");
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    set_session_status_for_test(&mut app, &session_id, SessionStatus::Merged);

    // Act
    let empty = app
        .drive_session_request(service.send_message(&missing_id, " \n "))
        .await;
    let missing = app
        .drive_session_request(service.send_message(&missing_id, "continue"))
        .await;
    let rejected = app
        .drive_session_request(service.send_message(&session_id, "continue"))
        .await;

    // Assert
    assert_eq!(
        empty,
        Err(SessionError::Operation(
            "Cannot send an empty session message".to_string()
        ))
    );
    assert_eq!(missing, Err(SessionError::NotFound));
    assert_eq!(
        rejected,
        Err(SessionError::Operation(format!(
            "Session `{session_id}` cannot accept a message in status `Merged`"
        )))
    );
    assert_eq!(
        app.sessions
            .session_for_id(&session_id)
            .expect("session")
            .queued_messages,
        []
    );
}
