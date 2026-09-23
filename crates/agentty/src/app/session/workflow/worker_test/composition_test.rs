use std::collections::VecDeque;
use std::future::{Future, poll_fn};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use ag_contracts::MockAgentChannel;
use tokio::sync::{Notify, mpsc};
use tokio::time;

use super::super::{SessionWorkerRuntime, SessionWorkerService};
use super::support::queue_test_context;
use crate::app::service::MockSessionRunFactory;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session::{QueuedMessage, Status};
use crate::test_support::{
    finish_session_creation_tasks, new_git_test_app_with_clients, new_test_app_with_clients,
    set_session_status_for_test, test_app_clients,
};

#[tokio::test]
async fn workers_reuse_channels_and_recompose_after_model_switch_and_shutdown() {
    // Arrange
    let creations = Arc::new(Mutex::new(Vec::new()));
    let captured_creations = Arc::clone(&creations);
    let (shutdown_tx, mut shutdown_rx) = mpsc::unbounded_channel();
    let mut factory = MockSessionRunFactory::new();
    factory.expect_create().times(3).returning(move |id, kind| {
        captured_creations
            .lock()
            .expect("creations")
            .push((id.clone(), kind));
        let mut channel = MockAgentChannel::new();
        let shutdown_tx = shutdown_tx.clone();
        channel
            .expect_shutdown_session()
            .once()
            .returning(move |id| {
                let shutdown_tx = shutdown_tx.clone();

                Box::pin(async move {
                    shutdown_tx.send(id).expect("shutdown observer");

                    Ok(())
                })
            });

        ag_worker::SessionRunClient::from_channel(id.to_string(), Arc::new(channel))
    });
    let clients = test_app_clients().with_session_run_factory(Arc::new(factory));
    let (app, _app_directory) = new_test_app_with_clients(clients).await;
    let (context, _database, _queue, _directory) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::Review).await;
    let mut runtime = SessionWorkerRuntime {
        branch_operation_lock: context.branch_operation_lock,
        cancel_token: context.cancel_token,
        child_pid: context.child_pid,
        folder: context.folder,
        personality_catalog_client: context.personality_catalog_client,
        queued_messages: context.queued_messages,
        queued_work_sequence: Arc::default(),
        review_request_client: context.review_request_client,
        session_agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt6Sol),
        session_id: "first".into(),
        session_update_versions: context.session_update_versions,
        status: context.status,
        transcript: context.transcript,
    };
    let mut workers = SessionWorkerService::new();

    // Act
    let first = workers.ensure_session_worker(&app.services, &runtime);
    let reused = workers.ensure_session_worker(&app.services, &runtime);

    // Assert
    assert_eq!(
        first.next_queued_work_order() + 1,
        reused.next_queued_work_order()
    );
    assert_eq!(creations.lock().expect("creations").len(), 1);

    // Act
    runtime.session_id = "second".into();
    let second = workers.ensure_session_worker(&app.services, &runtime);
    workers.clear_session_worker("first");
    drop(first);
    drop(reused);

    // Assert
    assert_eq!(
        time::timeout(Duration::from_secs(5), shutdown_rx.recv())
            .await
            .expect("first worker shutdown"),
        Some("first".to_string())
    );

    // Act: model switching clears the old worker before the next submission.
    runtime.session_id = "first".into();
    runtime.session_agent = AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus55);
    let replacement = workers.ensure_session_worker(&app.services, &runtime);
    drop(second);
    drop(replacement);
    workers.clear_session_worker("first");
    workers.clear_session_worker("second");

    // Assert
    assert_eq!(
        *creations.lock().expect("creations"),
        vec![
            ("first".into(), AgentKind::Codex),
            ("second".into(), AgentKind::Codex),
            ("first".into(), AgentKind::Claude),
        ]
    );
    let mut closed = Vec::new();
    for _ in 0..2 {
        closed.push(
            time::timeout(Duration::from_secs(5), shutdown_rx.recv())
                .await
                .expect("worker shutdown")
                .expect("shutdown notification"),
        );
    }
    closed.sort();
    assert_eq!(closed, ["first", "second"]);
}

#[tokio::test]
async fn model_switch_commits_before_retiring_runtime_and_discards_queue() {
    // Arrange
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let channel = gated_shutdown_channel(&started, &release);
    let mut factory = MockSessionRunFactory::new();
    factory.expect_create().once().returning(move |id, _| {
        ag_worker::SessionRunClient::from_channel(id.to_string(), channel.clone())
    });
    let clients = test_app_clients().with_session_run_factory(Arc::new(factory));
    let (mut app, _directory) = new_git_test_app_with_clients(clients).await;
    let session_id = app.create_session().await.expect("session");
    finish_session_creation_tasks(&mut app).await;
    set_session_status_for_test(&mut app, &session_id, Status::Question);
    let runtime = app
        .sessions
        .session_worker_runtime_or_err(&app.services, &session_id)
        .expect("runtime");
    let old_agent = runtime.session_agent;
    let new_agent = if old_agent.kind() == AgentKind::Claude {
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt6Sol)
    } else {
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus55)
    };
    let queued = runtime.queued_messages.clone();
    queued
        .lock()
        .expect("queue")
        .push_back(QueuedMessage::new(0, "pending old turn".into()));
    let retained = app
        .sessions
        .worker_service_mut()
        .ensure_session_worker(&app.services, &runtime);
    let database = app.services.db().clone();
    database
        .sessions()
        .update_session_provider_conversation_id(&session_id, Some("old-conversation".into()))
        .await
        .expect("conversation");

    // Act
    let mut switch = Box::pin(app.set_session_model(&session_id, new_agent));
    tokio::select! {
        () = started.notified() => {}
        result = &mut switch => panic!("switch completed before cleanup: {result:?}"),
    }

    // Assert: the switch is durable before pending work is discarded, while
    // the caller still waits for runtime cleanup.
    assert!(queued.lock().expect("queue").is_empty());
    let rows = database.sessions().load_sessions().await.expect("sessions");
    assert_eq!(
        rows.iter()
            .find(|row| row.id == session_id)
            .expect("session row")
            .model,
        new_agent.model().as_str()
    );
    assert_eq!(
        database
            .sessions()
            .get_session_provider_conversation_id(&session_id)
            .await
            .expect("conversation")
            .as_deref(),
        None
    );
    release.notify_one();
    switch.await.expect("model switch");
    assert_eq!(app.selected_session().expect("session").agent, new_agent);
    assert!(
        database
            .sessions()
            .get_session_provider_conversation_id(&session_id)
            .await
            .expect("conversation")
            .is_none()
    );
    drop(retained);
}

/// Scripted cleanup keeps the old runtime alive until the test releases it.
fn gated_shutdown_channel(started: &Arc<Notify>, release: &Arc<Notify>) -> Arc<MockAgentChannel> {
    let mut channel = MockAgentChannel::new();
    let cleanup_started = started.clone();
    let cleanup_release = release.clone();
    channel
        .expect_shutdown_session()
        .once()
        .returning(move |_| {
            let started = cleanup_started.clone();
            let release = cleanup_release.clone();
            Box::pin(async move {
                started.notify_one();
                release.notified().await;
                Ok(())
            })
        });
    Arc::new(channel)
}

#[tokio::test]
async fn failed_model_save_preserves_the_old_worker_and_pending_messages() {
    // Arrange
    let (mut app, _directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let id = app.create_session().await.expect("session");
    finish_session_creation_tasks(&mut app).await;
    set_session_status_for_test(&mut app, &id, Status::Question);
    let runtime = app
        .sessions
        .session_worker_runtime_or_err(&app.services, &id)
        .expect("runtime");
    let old_agent = runtime.session_agent;
    let new_agent = if old_agent.kind() == AgentKind::Claude {
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt6Sol)
    } else {
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus55)
    };
    runtime
        .queued_messages
        .lock()
        .expect("queue")
        .push_back(QueuedMessage::new(0, "pending".into()));
    let worker = app
        .sessions
        .worker_service_mut()
        .ensure_session_worker(&app.services, &runtime);
    let project = app
        .services
        .db()
        .sessions()
        .load_session_project_id(&id)
        .await
        .expect("project")
        .expect("project id");
    app.services
        .db()
        .settings()
        .upsert_project_setting(
            project,
            ag_session::SettingName::LastUsedModelAsDefault,
            "true",
        )
        .await
        .expect("defaults enabled");
    for rejection in [
        "CREATE TRIGGER reject_switch BEFORE UPDATE OF agent ON session BEGIN SELECT RAISE(ABORT, \
         'save failed'); END",
        "CREATE TRIGGER reject_switch BEFORE INSERT ON project_setting WHEN NEW.name = \
         'DefaultSmartModel' BEGIN SELECT RAISE(ABORT, 'save failed'); END",
    ] {
        sqlx::query(rejection)
            .execute(&pool)
            .await
            .expect("failure fixture");

        // Act
        assert!(app.set_session_model(&id, new_agent).await.is_err());

        // Assert: the original mailbox and queued message survive each failure.
        assert_eq!(app.selected_session().expect("session").agent, old_agent);
        assert_eq!(runtime.queued_messages.lock().expect("queue").len(), 1);
        assert!(
            app.sessions
                .worker_service_mut()
                .workers
                .contains_key(id.as_str())
        );
        let reused = app
            .sessions
            .worker_service_mut()
            .ensure_session_worker(&app.services, &runtime);
        assert_eq!(
            worker.next_queued_work_order() + 1,
            reused.next_queued_work_order()
        );
        sqlx::query("DROP TRIGGER reject_switch")
            .execute(&pool)
            .await
            .expect("repair fixture");
    }
    app.set_session_model(&id, new_agent)
        .await
        .expect("retry switch");
    assert_eq!(app.selected_session().expect("session").agent, new_agent);
    assert!(runtime.queued_messages.lock().expect("queue").is_empty());
}

#[tokio::test]
async fn model_switch_holds_scheduling_during_lookup_and_resumes_after_lookup_failure() {
    // Arrange: occupy the only database connection to block the first lookup.
    let (mut app, _directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let id = app.create_session().await.expect("session");
    finish_session_creation_tasks(&mut app).await;
    set_session_status_for_test(&mut app, &id, Status::Question);
    let runtime = app
        .sessions
        .session_worker_runtime_or_err(&app.services, &id)
        .expect("runtime");
    let old_agent = runtime.session_agent;
    let new_agent = if old_agent.kind() == AgentKind::Claude {
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt6Sol)
    } else {
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus55)
    };
    runtime
        .queued_messages
        .lock()
        .expect("queue")
        .push_back(QueuedMessage::new(0, "pending".into()));
    let worker = app
        .sessions
        .worker_service_mut()
        .ensure_session_worker(&app.services, &runtime);
    let connection = pool.acquire().await.expect("occupied connection");
    let mut switch = Box::pin(app.set_session_model(&id, new_agent));

    // Act: run until the database lookup blocks.
    poll_fn(|context| {
        assert!(switch.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;

    // Assert: a second scheduling hold cannot be acquired while the switch
    // waits for its lookup. This also prevents the worker from starting work.
    let mut competing_pause = Box::pin(worker.pause());
    poll_fn(|context| {
        assert!(competing_pause.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(competing_pause);

    // Act: fail the blocked lookup, without ever committing a selection change.
    drop(connection);
    let ((), result) = tokio::join!(pool.close(), switch);
    assert!(result.is_err());

    // Assert: the reversible hold is released and accepted work survives.
    let resumed = time::timeout(Duration::from_secs(1), worker.pause())
        .await
        .expect("pause released");
    assert_eq!(app.selected_session().expect("session").agent, old_agent);
    assert_eq!(runtime.queued_messages.lock().expect("queue").len(), 1);
    assert!(
        app.sessions
            .worker_service_mut()
            .workers
            .contains_key(id.as_str())
    );
    drop(resumed);
}
