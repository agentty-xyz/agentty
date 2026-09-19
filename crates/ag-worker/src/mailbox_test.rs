use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::task::{Context, Wake, Waker};

use async_trait::async_trait;
use tokio::sync::{Notify, mpsc, oneshot};

use crate::{
    ScheduledCommand, ScheduledWork, SessionWorkerHandle, WorkQueue, WorkerHost,
    test_session_worker_handle,
};

struct Command(u64);

impl ScheduledCommand for Command {
    fn order(&self) -> Option<u64> {
        Some(self.0)
    }

    fn can_run_while_paused(&self) -> bool {
        false
    }
}

struct Host {
    execution_gate: Option<Arc<Notify>>,
    messages: Mutex<VecDeque<u64>>,
    paused: Arc<AtomicBool>,
    results: mpsc::UnboundedSender<String>,
    shutdown_gate: Option<Arc<Notify>>,
}

impl WorkQueue for Host {
    type Command = Command;
    type Message = u64;

    fn paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    fn message_order(&self) -> Option<u64> {
        self.messages.lock().expect("messages").front().copied()
    }

    fn pop_message(&self) -> Option<u64> {
        self.messages.lock().expect("messages").pop_front()
    }
}

#[async_trait]
impl WorkerHost for Host {
    async fn execute(&self, work: ScheduledWork<Command, u64>) {
        let (ScheduledWork::Command(Command(order)) | ScheduledWork::Message(order)) = work;
        self.results
            .send(format!("start {order}"))
            .expect("observer");
        tokio::task::yield_now().await;
        if let Some(gate) = &self.execution_gate {
            gate.notified().await;
        }
        self.results
            .send(format!("finish {order}"))
            .expect("observer");
    }

    async fn abandon(&self, work: ScheduledWork<Command, u64>) {
        let (ScheduledWork::Command(Command(order)) | ScheduledWork::Message(order)) = work;
        self.results
            .send(format!("abandon {order}"))
            .expect("observer");
    }

    async fn shutdown(&self) {
        self.results.send("shutdown".into()).expect("observer");
        if let Some(gate) = &self.shutdown_gate {
            gate.notified().await;
        }
    }
}

#[tokio::test]
async fn cloned_handles_share_order_and_wake_serial_execution_through_post_processing() {
    // Arrange
    let sequence = Arc::new(AtomicU64::new(1));
    let paused = Arc::new(AtomicBool::new(true));
    let (results, mut observed) = mpsc::unbounded_channel();
    let worker = SessionWorkerHandle::spawn(
        Host {
            execution_gate: None,
            messages: Mutex::new(VecDeque::from([0])),
            paused: paused.clone(),
            results,
            shutdown_gate: None,
        },
        sequence.clone(),
    );
    let other = worker.clone();
    let first = worker.next_queued_work_order();
    let second = other.next_queued_work_order();

    // Act
    assert!(worker.submit(Command(first)).is_ok());
    assert!(other.submit(Command(second)).is_ok());
    tokio::task::yield_now().await;
    assert!(observed.try_recv().is_err());
    paused.store(false, Ordering::SeqCst);
    other.wake();
    let mut actual = Vec::new();
    for _ in 0..6 {
        actual.push(observed.recv().await.expect("executed work"));
    }
    drop(worker);
    drop(other);

    // Assert
    assert_eq!(sequence.load(Ordering::SeqCst), 3);
    assert_eq!(
        actual,
        [
            "start 0", "finish 0", "start 1", "finish 1", "start 2", "finish 2"
        ]
    );
    assert_eq!(observed.recv().await.as_deref(), Some("shutdown"));
    assert!(observed.recv().await.is_none());
}

#[tokio::test]
async fn closing_the_mailbox_abandons_paused_work_before_shutdown() {
    // Arrange
    let (results, mut observed) = mpsc::unbounded_channel();
    let worker = SessionWorkerHandle::spawn(
        Host {
            execution_gate: None,
            messages: Mutex::new(VecDeque::from([0])),
            paused: Arc::new(AtomicBool::new(true)),
            results,
            shutdown_gate: None,
        },
        Arc::default(),
    );

    // Act
    assert!(worker.submit(Command(1)).is_ok());
    drop(worker);

    // Assert
    assert_eq!(observed.recv().await.as_deref(), Some("abandon 1"));
    assert_eq!(observed.recv().await.as_deref(), Some("abandon 0"));
    assert_eq!(observed.recv().await.as_deref(), Some("shutdown"));
    assert!(observed.recv().await.is_none());
}

#[test]
fn submission_returns_ownership_when_the_worker_has_stopped() {
    // Arrange
    let (sender, receiver) = mpsc::unbounded_channel();
    let worker = test_session_worker_handle(Arc::default(), sender, Arc::default());
    drop(receiver);

    // Act
    let result = worker.submit(Command(7));

    // Assert
    assert_eq!(result.expect_err("closed worker").0.0, 7);
}

#[tokio::test]
async fn shutdown_finishes_active_effects_abandons_pending_work_and_waits_for_cleanup() {
    // Arrange
    let execution_gate = Arc::new(Notify::new());
    let shutdown_gate = Arc::new(Notify::new());
    let (results, mut observed) = mpsc::unbounded_channel();
    let worker = SessionWorkerHandle::spawn(
        Host {
            execution_gate: Some(execution_gate.clone()),
            messages: Mutex::new(VecDeque::from([2])),
            paused: Arc::new(AtomicBool::new(false)),
            results,
            shutdown_gate: Some(shutdown_gate.clone()),
        },
        Arc::default(),
    );
    let retained = worker.clone();
    assert!(worker.submit(Command(0)).is_ok());
    assert_eq!(observed.recv().await.as_deref(), Some("start 0"));
    assert!(worker.submit(Command(1)).is_ok());

    // Act
    let shutdown = tokio::spawn(worker.shutdown());
    retained.stop.cancelled().await;

    // Assert
    assert_eq!(
        retained
            .submit(Command(9))
            .expect_err("stopping worker")
            .0
            .0,
        9
    );
    assert!(!shutdown.is_finished());
    assert!(observed.try_recv().is_err());
    execution_gate.notify_one();
    for expected in ["finish 0", "abandon 1", "abandon 2", "shutdown"] {
        assert_eq!(observed.recv().await.as_deref(), Some(expected));
    }
    assert!(!shutdown.is_finished());
    assert!(retained.submit(Command(3)).is_err());
    shutdown_gate.notify_one();
    shutdown.await.expect("worker cleanup");
    assert!(observed.recv().await.is_none());
}

#[tokio::test]
async fn shutdown_wakes_an_idle_worker_even_with_a_retained_sender() {
    // Arrange
    let (results, mut observed) = mpsc::unbounded_channel();
    let worker = SessionWorkerHandle::spawn(
        Host {
            execution_gate: None,
            messages: Mutex::default(),
            paused: Arc::new(AtomicBool::new(false)),
            results,
            shutdown_gate: None,
        },
        Arc::default(),
    );
    let retained = worker.clone();
    tokio::task::yield_now().await;

    // Act
    worker.stop.cancel();
    assert_eq!(
        retained
            .submit(Command(0))
            .expect_err("stopping worker")
            .0
            .0,
        0
    );
    worker.shutdown().await;

    // Assert
    assert_eq!(observed.recv().await.as_deref(), Some("shutdown"));
    assert!(retained.submit(Command(1)).is_err());
    assert!(observed.recv().await.is_none());
}

#[tokio::test]
async fn dropping_a_pause_resumes_pending_commands_and_messages_after_active_effects() {
    // Arrange
    let gate = Arc::new(Notify::new());
    let (results, mut observed) = mpsc::unbounded_channel();
    let worker = SessionWorkerHandle::spawn(
        Host {
            execution_gate: Some(gate.clone()),
            messages: Mutex::new(VecDeque::from([2])),
            paused: Arc::new(AtomicBool::new(false)),
            results,
            shutdown_gate: None,
        },
        Arc::default(),
    );
    assert!(worker.submit(Command(0)).is_ok());
    assert_eq!(observed.recv().await.as_deref(), Some("start 0"));

    // Act: acquiring a pause waits for the active workflow, including effects.
    let mut pause = Box::pin(worker.pause());
    tokio::select! {
        _ = &mut pause => panic!("paused before active effects completed"),
        () = tokio::task::yield_now() => {}
    }
    gate.notify_one();
    let pause = pause.await;

    // Assert: pending work remains intact until a failed update drops the hold.
    assert_eq!(observed.recv().await.as_deref(), Some("finish 0"));
    assert!(observed.try_recv().is_err());
    tokio::task::yield_now().await;
    // A command arriving during the hold must precede the later message.
    assert!(worker.submit(Command(1)).is_ok());
    drop(pause);
    for order in [1, 2] {
        assert_eq!(observed.recv().await, Some(format!("start {order}")));
        gate.notify_one();
        assert_eq!(observed.recv().await, Some(format!("finish {order}")));
    }
    worker.shutdown().await;
    assert_eq!(observed.recv().await.as_deref(), Some("shutdown"));
}

#[tokio::test]
async fn retiring_a_pause_never_starts_pending_work_and_waits_for_cleanup() {
    // Arrange
    let gate = Arc::new(Notify::new());
    let (results, mut observed) = mpsc::unbounded_channel();
    let worker = SessionWorkerHandle::spawn(
        Host {
            execution_gate: None,
            messages: Mutex::new(VecDeque::from([1])),
            paused: Arc::new(AtomicBool::new(false)),
            results,
            shutdown_gate: Some(gate.clone()),
        },
        Arc::default(),
    );
    let pause = worker.pause().await;
    assert!(worker.submit(Command(0)).is_ok());

    // Act
    let retirement = tokio::spawn(pause.shutdown());

    // Assert
    for expected in ["abandon 0", "abandon 1", "shutdown"] {
        assert_eq!(observed.recv().await.as_deref(), Some(expected));
    }
    assert!(!retirement.is_finished());
    gate.notify_one();
    retirement.await.expect("retired");
    assert!(worker.submit(Command(2)).is_err());
}

#[tokio::test]
async fn submission_and_both_shutdown_paths_serialize_their_admission_boundary() {
    for retire_pause in [false, true] {
        // Arrange: a receiver wakeup holds send in progress while a cloned
        // handle or pause begins shutdown on a different thread.
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let worker = test_session_worker_handle(Arc::default(), sender, Arc::default());
        let pause = if retire_pause {
            Some(worker.pause().await)
        } else {
            None
        };
        let submission_finished = Arc::new(Barrier::new(2));
        let cancellation_observed = Arc::new(AtomicBool::new(false));
        let cancellation_waker = Waker::from(Arc::new(CallbackWake(Box::new({
            let admission = worker.admission.clone();
            let observed = cancellation_observed.clone();
            let submission_finished = submission_finished.clone();
            move || {
                // Once send has returned, only shutdown can hold this lock.
                submission_finished.wait();
                assert!(
                    admission.try_lock().is_err(),
                    "shutdown must hold admission"
                );
                observed.store(true, Ordering::SeqCst);
            }
        }))));
        let mut cancellation = Box::pin(worker.stop.cancelled());
        assert!(
            cancellation
                .as_mut()
                .poll(&mut Context::from_waker(&cancellation_waker))
                .is_pending()
        );
        let (entered, submission_entered) = oneshot::channel();
        let release = Arc::new(Barrier::new(2));
        let submission_waker = Waker::from(Arc::new(CallbackWake(Box::new({
            let admission = worker.admission.clone();
            let entered = Mutex::new(Some(entered));
            let release = release.clone();
            move || {
                assert!(admission.try_lock().is_err(), "send must hold admission");
                entered
                    .lock()
                    .expect("entered sender")
                    .take()
                    .expect("first wakeup")
                    .send(())
                    .expect("submission observer");
                release.wait();
            }
        }))));
        assert!(
            receiver
                .poll_recv(&mut Context::from_waker(&submission_waker))
                .is_pending()
        );

        // Act: overlap send and retirement, keeping send suspended until the
        // shutdown thread is ready to poll its future.
        let submission = tokio::task::spawn_blocking({
            let worker = worker.clone();
            move || {
                let result = worker.submit(Command(7));
                submission_finished.wait();
                result
            }
        });
        submission_entered.await.expect("send in progress");
        let (started, shutdown_started) = oneshot::channel();
        let shutdown = tokio::task::spawn_blocking({
            let worker = worker.clone();
            move || {
                let mut shutdown = Box::pin(async move {
                    if let Some(pause) = pause {
                        pause.shutdown().await;
                    } else {
                        worker.shutdown().await;
                    }
                });
                started.send(()).expect("shutdown observer");
                // The injected mailbox has an already-closed completion
                // channel, so only the synchronous admission lock can wait.
                assert!(
                    shutdown
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop()))
                        .is_ready()
                );
            }
        });
        shutdown_started.await.expect("shutdown started");
        tokio::task::spawn_blocking(move || {
            release.wait();
        })
        .await
        .expect("release submission");

        // Assert: the accepted send precedes cancellation; after cancellation
        // the still-open receiver cannot admit another command.
        assert!(submission.await.expect("submission thread").is_ok());
        shutdown.await.expect("shutdown thread");
        assert!(cancellation_observed.load(Ordering::SeqCst));
        assert_eq!(receiver.try_recv().expect("accepted command").0, 7);
        assert_eq!(worker.submit(Command(9)).expect_err("retired").0.0, 9);
        assert!(receiver.try_recv().is_err());
    }
}

struct CallbackWake(Box<dyn Fn() + Send + Sync>);

impl Wake for CallbackWake {
    fn wake(self: Arc<Self>) {
        (self.0)();
    }
}
