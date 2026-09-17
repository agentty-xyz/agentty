use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_runtime::MockAgentChannel;
use tokio::sync::mpsc;
use tokio::time;

use super::super::{SessionWorkerRuntime, SessionWorkerService};
use super::support::queue_test_context;
use crate::app::service::MockSessionChannelFactory;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session::Status;
use crate::test_support::{new_test_app_with_clients, test_app_clients};

#[tokio::test]
async fn workers_reuse_channels_and_recompose_after_model_switch_and_shutdown() {
    // Arrange
    let creations = Arc::new(Mutex::new(Vec::new()));
    let captured_creations = Arc::clone(&creations);
    let (shutdown_tx, mut shutdown_rx) = mpsc::unbounded_channel();
    let mut factory = MockSessionChannelFactory::new();
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

        Arc::new(channel)
    });
    let clients = test_app_clients().with_session_channel_factory(Arc::new(factory));
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
        session_agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
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
    assert!(Arc::ptr_eq(&first.wakeup, &reused.wakeup));
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
    runtime.session_agent = AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus5);
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
