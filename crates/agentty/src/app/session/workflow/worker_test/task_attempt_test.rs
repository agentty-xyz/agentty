use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{AgentRequestKind, MockAgentChannel};
use ag_protocol::QuestionItem;
use ag_session::{
    DurableMessageVisibility, SessionError, SessionId, SessionStatus, SubmitTaskRequest,
    TaskAttemptStatus,
};
use sqlx::SqlitePool;
use tokio::sync::{Notify, mpsc};

use super::support::successful_turn_result;
use crate::app::App;
use crate::app::test_support::TestSessionRunFactory;
use crate::test_support::{
    SessionFixtureBuilder, new_git_test_app_with_pool, new_test_app, set_session_status_for_test,
};

fn request(attempt_id: &str) -> SubmitTaskRequest {
    SubmitTaskRequest {
        attempt_id: attempt_id.to_string(),
        message: "Advance the task".to_string(),
        objective: "Deliver the requested feature".to_string(),
        task_id: "task".to_string(),
        visibility: DurableMessageVisibility::Visible,
    }
}

#[tokio::test]
async fn task_api_runs_explicit_attempts_through_worker_and_keeps_completion_explicit() {
    // Arrange
    let (mut app, _directory, pool) = new_git_test_app_with_pool().await;
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    set_session_status_for_test(&mut app, &session_id, SessionStatus::Review);
    let factory = TestSessionRunFactory::install(&mut app.services);
    let release = Arc::new(Notify::new());
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let channel = task_channel(Arc::clone(&release), started_tx);
    factory.register(&session_id, Arc::new(channel));
    let service = app.session_service();

    for attempt_id in ["first", "continuation"] {
        let request = request(attempt_id);

        // Act
        app.drive_session_request(service.submit_task(&session_id, request.clone()))
            .await
            .expect("admit");
        let kind = tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
            .await
            .expect("start guard")
            .expect("start");
        app.drive_session_request(service.submit_task(&session_id, request.clone()))
            .await
            .expect("duplicate while busy");
        let mut conflicting = request.clone();
        conflicting.message = "Changed delivery".to_string();
        assert!(
            app.drive_session_request(service.submit_task(&session_id, conflicting))
                .await
                .is_err()
        );
        let running = app
            .drive_session_request(service.get_task("task"))
            .await
            .expect("load")
            .expect("task");
        let complete_while_running = app
            .drive_session_request(service.complete_task("task"))
            .await;

        // Assert
        assert_eq!(kind, AgentRequestKind::SessionResume);
        assert_eq!(
            running.attempts.last().expect("attempt").status,
            TaskAttemptStatus::Running
        );
        assert!(complete_while_running.is_err());

        // Act: a worker pause acknowledges only after ordered post-processing.
        release.notify_one();
        let paused = tokio::time::timeout(
            Duration::from_secs(10),
            app.sessions.pause_session_worker(&session_id),
        )
        .await
        .expect("settle guard")
        .expect("worker");
        let settled = app
            .drive_session_request(service.get_task("task"))
            .await
            .expect("load")
            .expect("task");
        let session = app
            .drive_session_request(service.get_session(&session_id))
            .await
            .expect("session")
            .expect("present");

        // Assert
        assert_eq!(settled.completed_at, None);
        assert_eq!(
            settled.attempts.last().expect("attempt").status,
            TaskAttemptStatus::Succeeded
        );
        assert_eq!(session.status, SessionStatus::Question);
        assert_eq!(session.questions, vec![QuestionItem::new("Which branch?")]);
        assert_eq!(
            session
                .messages
                .iter()
                .filter(|message| message.content == request.message)
                .count(),
            settled.attempts.len()
        );
        assert_eq!(session.queued_messages, [] as [String; 0]);
        set_session_status_for_test(&mut app, &session_id, SessionStatus::Question);
        drop(paused);
    }

    // Act
    app.drive_session_request(service.complete_task("task"))
        .await
        .expect("explicit acceptance");
    let task = app
        .drive_session_request(service.get_task("task"))
        .await
        .expect("load")
        .expect("task");
    app.drive_session_request(service.submit_task(&session_id, request("first")))
        .await
        .expect("old receipt");
    let rejected = app
        .drive_session_request(service.submit_task(&session_id, request("after-completion")))
        .await;

    // Assert
    assert!(task.completed_at.is_some());
    assert_eq!(task.attempts.len(), 2);
    assert_ne!(task.attempts[0].operation_id, task.attempts[1].operation_id);
    assert!(rejected.is_err());

    assert_task_storage_failure(&mut app, pool, &session_id).await;
}

fn task_channel(
    release: Arc<Notify>,
    started_tx: mpsc::UnboundedSender<AgentRequestKind>,
) -> MockAgentChannel {
    let mut channel = MockAgentChannel::new();
    channel
        .expect_run_turn()
        .times(2)
        .returning(move |_, turn, _| {
            let release = Arc::clone(&release);
            started_tx.send(turn.request_kind).expect("observe start");

            Box::pin(async move {
                release.notified().await;
                let mut result = successful_turn_result("Need clarification");
                result.assistant_message.questions = vec![QuestionItem::new("Which branch?")];

                Ok(result)
            })
        });
    channel
        .expect_shutdown_session()
        .returning(|_| Box::pin(async { Ok(()) }));

    channel
}

async fn assert_task_storage_failure(app: &mut App, pool: SqlitePool, session_id: &SessionId) {
    // Arrange
    let service = app.session_service();

    // Act / Assert: storage failures cross both query and admission replies.
    pool.close().await;
    assert!(
        app.drive_session_request(service.get_task("task"))
            .await
            .is_err()
    );
    assert!(
        app.drive_session_request(service.submit_task(session_id, request("storage-failure")))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn task_api_rejects_missing_busy_and_unbound_sessions_without_creating_work() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let service = app.session_service();
    let session_id = SessionId::from("session-id");

    // Act / Assert
    assert_eq!(
        app.drive_session_request(service.submit_task(&session_id, request("missing")))
            .await,
        Err(SessionError::NotFound)
    );
    app.sessions.push_session(
        SessionFixtureBuilder::new()
            .status(SessionStatus::InProgress)
            .build(),
    );
    assert!(
        app.drive_session_request(service.submit_task(&session_id, request("busy")))
            .await
            .is_err()
    );
    set_session_status_for_test(&mut app, &session_id, SessionStatus::Review);
    assert!(
        app.drive_session_request(service.submit_task(&session_id, request("unbound")))
            .await
            .is_err()
    );
    assert_eq!(
        app.drive_session_request(service.get_task("task"))
            .await
            .expect("load"),
        None
    );
    assert!(
        !app.sessions
            .reply_to_task(&app.services, "missing", request("missing"))
            .await
    );
}
