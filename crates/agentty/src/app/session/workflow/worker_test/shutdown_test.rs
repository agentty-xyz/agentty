use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use ag_contracts::MockAgentChannel;
use ag_telemetry::{KeyValue, QueuedTrace};
use ag_worker::WorkerHost as _;
use opentelemetry::global;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tokio::sync::{Notify, mpsc, oneshot};

use super::super::{ScheduledSessionCommand, SessionCommand, SessionWorkerHost};
use super::support::{
    auto_commit_run_client, insert_in_progress_test_session, queue_test_context, queued_message,
    queued_review_request_command,
};
use crate::app::AppEvent;
use crate::domain::session::Status;
use crate::infra::db::AppRepositories;
use crate::test_support::telemetry::TRACER_PROVIDER_LOCK;

#[tokio::test]
async fn shutdown_releases_only_the_stopped_sessions_queued_traces() {
    // Arrange
    let _provider_guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = InMemorySpanExporter::default();
    global::set_tracer_provider(
        SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build(),
    );
    let mut channel = MockAgentChannel::new();
    channel
        .expect_shutdown_session()
        .once()
        .returning(|_| Box::pin(async { Ok(()) }));
    let (context, _db, _queue, _directory) =
        queue_test_context(channel, VecDeque::new(), Status::Question).await;
    let stopped_key = (context.session_id.clone(), 1);
    let other_key = ("other-session".into(), 2);
    let message_traces = Arc::new(Mutex::new(HashMap::from([
        (
            stopped_key.clone(),
            QueuedTrace::new("test.stopped.turn", Vec::new()),
        ),
        (
            other_key.clone(),
            QueuedTrace::new("test.other.turn", Vec::new()),
        ),
    ])));
    let host = SessionWorkerHost {
        context,
        message_traces: Arc::clone(&message_traces),
        run_client: auto_commit_run_client(),
    };

    // Act
    host.shutdown().await;

    // Assert
    let traces = message_traces.lock().expect("message traces");
    assert!(!traces.contains_key(&stopped_key));
    assert!(traces.contains_key(&other_key));
    let spans = exporter.get_finished_spans().expect("spans");
    let stopped = spans
        .iter()
        .find(|span| span.name == "test.stopped.turn")
        .expect("canceled queued trace");
    assert!(
        stopped
            .attributes
            .contains(&KeyValue::new("agentty.outcome", "canceled"))
    );
    assert!(!spans.iter().any(|span| span.name == "test.other.turn"));
}

#[tokio::test]
async fn closed_worker_settles_paused_operations_and_notifies_callers() {
    for tracking_failure in [false, true] {
        // Arrange
        let mut channel = MockAgentChannel::new();
        channel
            .expect_shutdown_session()
            .once()
            .returning(|_| Box::pin(async { Ok(()) }));
        let (mut context, _db, queue, _directory) = queue_test_context(
            channel,
            VecDeque::from([queued_message(3, "waiting prompt")]),
            Status::Question,
        )
        .await;
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("database");
        insert_in_progress_test_session(&db).await;
        context.db = db.clone();
        for (id, kind) in [
            ("op-review-request", "create_review_request"),
            ("op-rebase", "rebase"),
        ] {
            db.operations()
                .insert_session_operation(id, "sess1", kind)
                .await
                .expect("accepted operation");
        }
        if tracking_failure {
            sqlx::query(
                "CREATE TRIGGER reject_cancel BEFORE UPDATE OF status ON session_operation WHEN \
                 NEW.status = 'canceled' BEGIN SELECT RAISE(ABORT, 'tracking unavailable'); END",
            )
            .execute(&pool)
            .await
            .expect("inject tracking failure");
        }
        let (response_tx, response_rx) = oneshot::channel();
        let mut review = queued_review_request_command(context.folder.clone());
        if let SessionCommand::CreateReviewRequest { response, .. } = &mut review {
            *response = Some(Arc::new(Mutex::new(Some(response_tx))));
        }
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        context.app_event_tx = event_tx;
        let host = SessionWorkerHost {
            message_traces: Arc::default(),
            context,
            run_client: auto_commit_run_client(),
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        assert!(
            sender
                .send(ScheduledSessionCommand::queued(review, 1))
                .is_ok()
        );
        assert!(
            sender
                .send(ScheduledSessionCommand::queued(
                    SessionCommand::Rebase {
                        operation_id: "op-rebase".into(),
                        base_branch: "main".into()
                    },
                    2
                ))
                .is_ok()
        );
        drop(sender);

        // Act
        ag_worker::run(host, Arc::new(Notify::new()), receiver).await;
        let response = response_rx.await.expect("caller notified");
        let operations = db
            .operations()
            .load_unfinished_session_operations()
            .await
            .expect("operation records");
        let events: Vec<_> = std::iter::from_fn(|| event_rx.try_recv().ok()).collect();

        // Assert
        assert!(response.is_err());
        assert_eq!(operations.len(), if tracking_failure { 2 } else { 0 });
        assert!(queue.lock().expect("queue").is_empty());
        assert!(matches!(
            events.as_slice(),
            [
                AppEvent::BranchPublishActionResolved { .. },
                AppEvent::SessionQueuedSyncResolved { .. },
                AppEvent::SessionUpdated { .. }
            ]
        ));
    }
}
