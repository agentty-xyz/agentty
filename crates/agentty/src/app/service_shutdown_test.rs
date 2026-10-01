use std::future;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ag_telemetry::QueuedTrace;
use ag_worker::{ScheduledCommand, ScheduledWork, SessionWorkerHandle, WorkQueue, WorkerHost};
use tokio::time;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::test_support::FixedClock;

struct Command(QueuedTrace);

impl ScheduledCommand for Command {
    fn order(&self) -> Option<u64> {
        Some(0)
    }

    fn can_run_while_paused(&self) -> bool {
        false
    }
}

struct Host {
    entered: CancellationToken,
    paused: bool,
    _released: DropGuard,
}

impl WorkQueue for Host {
    type Command = Command;
    type Message = ();

    fn paused(&self) -> bool {
        self.paused
    }

    fn message_order(&self) -> Option<u64> {
        None
    }

    fn pop_message(&self) -> Option<()> {
        None
    }
}

#[async_trait::async_trait]
impl WorkerHost for Host {
    async fn execute(&self, work: ScheduledWork<Command, ()>) {
        if let ScheduledWork::Command(command) = work {
            command
                .0
                .selected()
                .scope(async {
                    self.entered.cancel();
                    future::pending::<()>().await;
                })
                .await;
        }
    }

    async fn abandon(&self, _work: ScheduledWork<Command, ()>) {}

    async fn shutdown(&self) {
        self.entered.cancel();
        future::pending::<()>().await;
    }
}

#[tokio::test]
async fn cleanup_deadline_stops_session_execution_and_stuck_host_cleanup() {
    for paused in [false, true] {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_test_app().await;
        let entered = CancellationToken::new();
        let released = CancellationToken::new();
        let worker = SessionWorkerHandle::spawn(
            Host {
                entered: entered.clone(),
                paused,
                _released: released.clone().drop_guard(),
            },
            Arc::default(),
        );
        let task = worker.task();
        app.services.track_session_worker(task.clone());
        assert!(
            worker
                .submit(Command(QueuedTrace::new("test.shutdown", Vec::new())))
                .is_ok()
        );
        if paused {
            task.request_shutdown();
        }
        entered.cancelled().await;
        app.services.clock = Arc::new(FixedClock::new(
            time::Instant::now()
                .into_std()
                .checked_sub(Duration::from_secs(6))
                .expect("past deadline"),
            SystemTime::UNIX_EPOCH,
        ));

        // Act
        time::timeout(
            Duration::from_secs(1),
            app.wait_for_background_cleanup_tasks(),
        )
        .await
        .expect("shared deadline");

        // Assert
        assert!(task.is_finished());
        assert!(released.is_cancelled());
        assert!(
            app.services
                .session_worker_tasks
                .lock()
                .expect("tasks")
                .is_empty()
        );

        // Act: reaping completed observers must retain only unfinished tasks.
        app.services.track_session_worker(task.clone());
        app.services.track_session_worker(task);

        // Assert
        assert_eq!(
            app.services
                .session_worker_tasks
                .lock()
                .expect("tasks")
                .len(),
            1
        );
        app.services.wait_for_cleanup_tasks(None).await;
    }
}
