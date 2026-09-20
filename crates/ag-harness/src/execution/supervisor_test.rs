use std::collections::VecDeque;
use std::future::{Future, pending};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot, watch};

use super::{Backend, Clock, Control, Event, Process, Running, Supervisor, Worker};
use crate::execution::contract::{
    Command, Execution, ExecutionControl, ExecutionError, Executor, Grants, Limits, MainExit,
    Policy, PreparedExecution, Stream, Termination,
};

#[test]
fn construction_without_a_runtime_rejects_before_binding() {
    // Arrange
    let fixture = Fixture::ready();
    assert!(Handle::try_current().is_err());

    // Act
    let result = Supervisor::new(fixture.backend.clone(), Arc::new(TestClock));

    // Assert
    assert_eq!(result.err(), Some(ExecutionError::Supervision));
    assert!(fixture.backend.0.lock().expect("backend owner").is_some());
    assert_eq!(fixture.state.dropped.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn preparation_outside_an_entered_runtime_retains_cleanup_control() {
    // Arrange
    let fixture = Fixture::ready();
    fixture.complete();
    let supervisor =
        Supervisor::new(fixture.backend.clone(), Arc::new(TestClock)).expect("supervisor runtime");
    let limits = limits(0);

    // Act
    let PreparedExecution { control, execution } = std::thread::spawn(move || {
        assert!(Handle::try_current().is_err());

        supervisor.prepare(command(), policy(), limits)
    })
    .join()
    .expect("preparation must not panic outside the runtime")
    .expect("inert binding");
    let result = execution.run().await;

    // Assert
    assert_eq!(result.termination, Termination::Completed);
    assert_eq!(result.execution_failure, None);
    assert_eq!(result.cleanup_failure, None);
    assert_eq!(control.cleanup().await, Ok(()));
    assert!(fixture.state.released.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn inert_binding_rejects_without_starting() {
    // Arrange
    let fixture = Fixture::new(Mode::Ready, Mode::Ready, vec![]);
    let supervisor =
        Supervisor::new(Arc::new(RejectBackend), Arc::new(TestClock)).expect("supervisor runtime");

    // Act
    let rejected = supervisor.prepare(command(), policy(), limits(10));
    let prepared = fixture.prepare(10);

    // Assert
    assert_eq!(rejected.err(), Some(ExecutionError::Unsupported));
    tokio::task::yield_now().await;
    assert_eq!(fixture.state.preparing.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.state.starting.load(Ordering::SeqCst), 0);
    drop(prepared.execution);
    assert_eq!(prepared.control.cleanup().await, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn completion_requires_main_exit_both_eofs_and_quiescence() {
    // Arrange
    let fixture = Fixture::ready();
    fixture.output(Stream::Stdout, &[0, 255]);
    fixture.output(Stream::Stderr, b"error");
    fixture.event(Event::MainExit(MainExit::Code(7)));
    fixture.event(Event::Eof(Stream::Stdout));
    fixture.event(Event::Eof(Stream::Stderr));
    let PreparedExecution { control, execution } = fixture.prepare(5);
    let running = tokio::spawn(execution.run());

    // Act
    wait_count(&fixture.state.reading, 6).await;

    // Assert
    assert!(!running.is_finished());
    assert_eq!(fixture.state.cleaning.load(Ordering::SeqCst), 0);

    // Act
    fixture.event(Event::Quiescent);
    let result = running.await.expect("supervised result");

    // Assert
    assert_eq!(result.termination, Termination::Completed);
    assert_eq!(result.main_exit, MainExit::Code(7));
    assert_eq!(result.output.stdout(), &[0, 255]);
    assert_eq!(result.output.stderr(), b"err");
    assert!(result.output.truncated());
    assert_eq!(result.execution_failure, None);
    assert_eq!(result.cleanup_failure, None);
    assert_eq!(control.cleanup().await, Ok(()));
    assert!(fixture.state.released.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn missing_descendants_pipe_or_main_observation_hits_the_shared_deadline() {
    for missing in 0..4 {
        // Arrange
        let fixture = Fixture::ready();
        for (index, event) in completion_events().into_iter().enumerate() {
            if index != missing {
                fixture.event(event);
            }
        }
        let PreparedExecution { control, execution } = fixture.prepare(0);

        // Act
        let result = execution.run().await;

        // Assert
        assert_eq!(result.termination, Termination::Deadline);
        assert_eq!(result.execution_failure, None);
        assert_eq!(result.cleanup_failure, None);
        assert_eq!(control.cleanup().await, Ok(()));
    }
}

#[tokio::test(start_paused = true)]
async fn stalled_preparation_and_partial_start_obey_cancellation_and_deadline() {
    for during_start in [false, true] {
        for cancel in [false, true] {
            // Arrange
            let (prepare, start) = if during_start {
                (Mode::Ready, Mode::Stall)
            } else {
                (Mode::Stall, Mode::Ready)
            };
            let fixture = Fixture::new(prepare, start, vec![]);
            let PreparedExecution { control, execution } = fixture.prepare(0);
            let running = tokio::spawn(execution.run());
            let reached = if during_start {
                &fixture.state.starting
            } else {
                &fixture.state.preparing
            };
            wait_count(reached, 1).await;

            // Act
            if cancel {
                control.cancel();
            }
            let result = running.await.expect("bounded stalled phase");

            // Assert
            assert_eq!(
                result.termination,
                if cancel {
                    Termination::Cancelled
                } else {
                    Termination::Deadline
                }
            );
            assert_eq!(result.execution_failure, None);
            assert_eq!(result.main_exit, MainExit::Unavailable);
            assert!(fixture.state.released.load(Ordering::SeqCst));
            assert_eq!(fixture.state.reading.load(Ordering::SeqCst), 0);
            assert_eq!(control.cleanup().await, Ok(()));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn preparation_and_start_failures_keep_the_original_error_when_cleanup_fails() {
    for during_start in [false, true] {
        // Arrange
        let (prepare, start) = if during_start {
            (Mode::Ready, Mode::Fail(ExecutionError::Process))
        } else {
            (Mode::Fail(ExecutionError::Setup), Mode::Ready)
        };
        let fixture = Fixture::new(prepare, start, vec![Mode::Fail(ExecutionError::Cleanup); 2]);
        let PreparedExecution { control, execution } = fixture.prepare(1);

        // Act
        let result = execution.run().await;

        // Assert
        assert_eq!(result.termination, Termination::Failed);
        assert_eq!(
            result.execution_failure,
            Some(if during_start {
                ExecutionError::Process
            } else {
                ExecutionError::Setup
            })
        );
        assert_eq!(
            result.cleanup_failure,
            Some(ExecutionError::CleanupUnconfirmed)
        );
        assert_eq!(fixture.state.cleaning.load(Ordering::SeqCst), 2);
        assert!(!fixture.state.released.load(Ordering::SeqCst));
        assert_eq!(control.cleanup().await, Ok(()));
        assert_eq!(fixture.state.cleaning.load(Ordering::SeqCst), 3);
        assert_eq!(
            result.cleanup_failure,
            Some(ExecutionError::CleanupUnconfirmed)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn event_failure_keeps_main_exit_and_truncated_output_with_unconfirmed_cleanup() {
    // Arrange
    let fixture = Fixture::new(Mode::Ready, Mode::Ready, vec![Mode::Stall; 2]);
    fixture.event(Event::MainExit(MainExit::Signal(9)));
    fixture.output(Stream::Stderr, b"diagnostic");
    fixture.send(Input::Failure);
    let PreparedExecution { control, execution } = fixture.prepare(4);
    let before = TestClock.now();

    // Act
    let result = execution.run().await;

    // Assert
    assert_eq!(TestClock.now() - before, Duration::from_secs(2));
    assert_eq!(result.main_exit, MainExit::Signal(9));
    assert_eq!(result.termination, Termination::Failed);
    assert_eq!(result.execution_failure, Some(ExecutionError::Process));
    assert_eq!(
        result.cleanup_failure,
        Some(ExecutionError::CleanupUnconfirmed)
    );
    assert_eq!(result.output.stderr(), b"diag");
    assert!(result.output.truncated());
    assert_eq!(fixture.state.cleaning.load(Ordering::SeqCst), 2);
    assert_eq!(control.cleanup().await, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn successful_exit_is_preserved_when_resource_cleanup_cannot_be_confirmed() {
    // Arrange
    let fixture = Fixture::new(
        Mode::Ready,
        Mode::Ready,
        vec![Mode::Fail(ExecutionError::Cleanup); 2],
    );
    fixture.complete();
    let PreparedExecution { control, execution } = fixture.prepare(0);

    // Act
    let result = execution.run().await;

    // Assert
    assert_eq!(result.main_exit, MainExit::Code(0));
    assert_eq!(result.termination, Termination::Completed);
    assert_eq!(result.execution_failure, None);
    assert_eq!(
        result.cleanup_failure,
        Some(ExecutionError::CleanupUnconfirmed)
    );
    drop(control);
    wait_count(&fixture.state.dropped, 1).await;
}

#[tokio::test(start_paused = true)]
async fn output_flood_drains_after_capture_exhaustion_without_blocking_cancellation() {
    // Arrange
    let fixture = Fixture::ready();
    fixture.state.flood.store(true, Ordering::SeqCst);
    let PreparedExecution { control, execution } = fixture.prepare(3);
    let mut running = execution.run();
    poll_pending(running.as_mut());

    // Act
    wait_count(&fixture.state.reading, 200).await;
    control.cancel();
    let result = running.await;

    // Assert
    assert_eq!(result.termination, Termination::Cancelled);
    assert_eq!(result.output.stdout(), &[255; 3]);
    assert!(result.output.truncated());
    assert_eq!(result.cleanup_failure, None);
    assert!(fixture.state.released.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn output_flood_obeys_deadline_without_an_active_result_consumer() {
    // Arrange
    let fixture = Fixture::ready();
    fixture.state.flood.store(true, Ordering::SeqCst);
    let PreparedExecution { control, execution } = fixture.prepare(0);
    let mut running = execution.run();
    poll_pending(running.as_mut());
    wait_count(&fixture.state.reading, 20).await;

    // Act
    tokio::time::advance(Duration::from_secs(10)).await;
    let result = running.await;

    // Assert
    assert_eq!(result.termination, Termination::Deadline);
    assert_eq!(result.output.stdout(), b"");
    assert!(result.output.truncated());
    assert_eq!(control.cleanup().await, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn dropping_callers_in_each_phase_preserves_automatic_cleanup_and_settlement() {
    for phase in 0..3 {
        // Arrange
        let fixture = Fixture::new(
            if phase == 0 { Mode::Stall } else { Mode::Ready },
            if phase == 1 { Mode::Stall } else { Mode::Ready },
            vec![],
        );
        let PreparedExecution { control, execution } = fixture.prepare(0);
        let mut running = execution.run();
        poll_pending(running.as_mut());
        let reached = match phase {
            0 => &fixture.state.preparing,
            1 => &fixture.state.starting,
            _ => &fixture.state.reading,
        };
        wait_count(reached, 1).await;

        // Act
        drop(running);
        wait_count(&fixture.state.cleaning, 1).await;

        // Assert
        assert!(fixture.state.released.load(Ordering::SeqCst));
        assert_eq!(control.cleanup().await, Ok(()));
    }
}

#[tokio::test(start_paused = true)]
async fn never_started_and_precancelled_callers_do_not_prepare_resources() {
    for action in 0..4 {
        // Arrange
        let fixture = Fixture::ready();
        let PreparedExecution { control, execution } = fixture.prepare(0);

        // Act
        match action {
            0 => drop(execution),
            1 => drop(execution.run()),
            2 => {
                control.cancel();
                control.cancel();
                assert_eq!(execution.run().await.termination, Termination::Cancelled);
            }
            _ => {
                tokio::time::advance(Duration::from_secs(10)).await;
                assert_eq!(execution.run().await.termination, Termination::Deadline);
            }
        }

        // Assert
        assert_eq!(control.cleanup().await, Ok(()));
        assert_eq!(fixture.state.preparing.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test(start_paused = true)]
async fn dropped_cleanup_waiter_and_concurrent_retries_leave_worker_ownership_intact() {
    // Arrange
    let fixture = Fixture::new(Mode::Stall, Mode::Ready, vec![Mode::Stall; 2]);
    let PreparedExecution { control, execution } = fixture.prepare(0);
    let running = tokio::spawn(execution.run());
    wait_count(&fixture.state.preparing, 1).await;
    let mut cleanup = control.cleanup();

    // Act
    poll_pending(cleanup.as_mut());
    drop(cleanup);
    let result = running.await.expect("cleanup worker survives waiter drop");
    let (first, second) = tokio::join!(control.cleanup(), control.cleanup());

    // Assert
    assert_eq!(result.termination, Termination::Cancelled);
    assert_eq!(
        result.cleanup_failure,
        Some(ExecutionError::CleanupUnconfirmed)
    );
    assert_eq!(first, Ok(()));
    assert_eq!(second, Ok(()));
    assert_eq!(fixture.state.cleaning.load(Ordering::SeqCst), 3);
    assert_eq!(control.cleanup().await, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn staggered_cleanup_retries_receive_their_own_settlements() {
    // Arrange
    let fixture = Fixture::staggered_cleanup();
    fixture.complete();
    let PreparedExecution { control, execution } = fixture.prepare(0);
    let result = execution.run().await;
    assert_eq!(
        result.cleanup_failure,
        Some(ExecutionError::CleanupUnconfirmed)
    );
    let mut first = control.cleanup();
    poll_pending(first.as_mut());
    wait_count(&fixture.state.cleaning, 3).await;
    let mut second = control.cleanup();
    poll_pending(second.as_mut());
    let mut third = control.cleanup();
    poll_pending(third.as_mut());

    // Act
    tokio::time::advance(Duration::from_secs(1)).await;
    wait_count(&fixture.state.cleaning, 4).await;
    tokio::time::advance(Duration::from_secs(1)).await;
    wait_count(&fixture.state.cleaning, 5).await;

    // Assert
    poll_pending(second.as_mut());
    assert_eq!(second.await, Ok(()));
    assert_eq!(third.await, Ok(()));
    assert_eq!(first.await, Err(ExecutionError::CleanupUnconfirmed));
    assert_eq!(fixture.state.cleaning.load(Ordering::SeqCst), 5);
    assert_eq!(control.cleanup().await, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn dropping_a_queued_retry_waiter_does_not_abandon_its_cleanup() {
    // Arrange
    let fixture = Fixture::staggered_cleanup();
    fixture.complete();
    let PreparedExecution { control, execution } = fixture.prepare(0);
    let result = execution.run().await;
    assert_eq!(
        result.cleanup_failure,
        Some(ExecutionError::CleanupUnconfirmed)
    );
    let mut first = control.cleanup();
    poll_pending(first.as_mut());
    wait_count(&fixture.state.cleaning, 3).await;
    let mut second = control.cleanup();
    poll_pending(second.as_mut());

    // Act
    drop(second);
    tokio::time::advance(Duration::from_secs(1)).await;
    wait_count(&fixture.state.cleaning, 4).await;
    tokio::time::advance(Duration::from_secs(1)).await;
    wait_count(&fixture.state.cleaning, 5).await;
    tokio::time::advance(Duration::from_millis(100)).await;
    wait_count(&fixture.state.dropped, 1).await;

    // Assert
    assert_eq!(first.await, Err(ExecutionError::CleanupUnconfirmed));
    assert!(fixture.state.released.load(Ordering::SeqCst));
    assert_eq!(control.cleanup().await, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn closed_retry_request_or_reply_channels_leave_cleanup_unconfirmed() {
    for accepted in [false, true] {
        // Arrange
        let (cancel, _) = watch::channel(false);
        let (_settlement, settled) = watch::channel(Some(Err(ExecutionError::CleanupUnconfirmed)));
        let (retry, mut retries) = mpsc::channel(1);
        let control = Control {
            cancel,
            retry,
            settled,
        };
        if !accepted {
            retries.close();
        }
        let worker = tokio::spawn(async move {
            if accepted {
                let reply = retries.recv().await.expect("retry request");
                drop(reply);
            }
        });

        // Act
        let result = control.cleanup().await;
        worker.await.expect("closed retry worker");

        // Assert
        assert_eq!(result, Err(ExecutionError::CleanupUnconfirmed));
    }
}

#[tokio::test(start_paused = true)]
async fn cleanup_timeout_can_recover_within_the_bounded_attempts() {
    // Arrange
    let fixture = Fixture::new(Mode::Ready, Mode::Ready, vec![Mode::Stall, Mode::Ready]);
    fixture.complete();
    let PreparedExecution { control, execution } = fixture.prepare(0);
    let before = TestClock.now();

    // Act
    let result = execution.run().await;

    // Assert
    assert_eq!(TestClock.now() - before, Duration::from_secs(1));
    assert_eq!(result.cleanup_failure, None);
    assert_eq!(fixture.state.cleaning.load(Ordering::SeqCst), 2);
    assert_eq!(control.cleanup().await, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn backend_length_violation_fails_without_indexing_outside_the_read_buffer() {
    // Arrange
    let fixture = Fixture::ready();
    fixture.event(Event::Output(Stream::Stdout, usize::MAX));
    let PreparedExecution { control, execution } = fixture.prepare(0);

    // Act
    let result = execution.run().await;

    // Assert
    assert_eq!(result.termination, Termination::Failed);
    assert_eq!(result.execution_failure, Some(ExecutionError::Process));
    assert_eq!(control.cleanup().await, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn losing_all_callers_does_not_abandon_resources() {
    // Arrange
    let fixture = Fixture::ready();
    let PreparedExecution { control, execution } = fixture.prepare(0);
    let mut running = execution.run();
    poll_pending(running.as_mut());
    wait_count(&fixture.state.reading, 1).await;

    // Act
    drop(control);
    drop(running);
    wait_count(&fixture.state.dropped, 1).await;

    // Assert
    assert!(fixture.state.released.load(Ordering::SeqCst));
    assert_eq!(fixture.state.cleaning.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn closed_worker_channels_report_failure_instead_of_success() {
    // Arrange
    let (cancel, _) = watch::channel(false);
    let (result, outcome) = oneshot::channel();
    let running = Box::new(Running {
        cancel: cancel.clone(),
        limits: limits(0),
        outcome,
        start: None,
    });
    let (settlement, settled) = watch::channel(None);
    let (retry, _) = mpsc::channel(1);
    let control = Control {
        cancel,
        retry,
        settled,
    };
    drop(result);
    drop(settlement);

    // Act
    let result = running.run().await;

    // Assert
    assert_eq!(result.termination, Termination::Failed);
    assert_eq!(result.execution_failure, Some(ExecutionError::Supervision));
    assert_eq!(
        result.cleanup_failure,
        Some(ExecutionError::CleanupUnconfirmed)
    );
    assert_eq!(
        control.cleanup().await,
        Err(ExecutionError::CleanupUnconfirmed)
    );
}

#[tokio::test(start_paused = true)]
async fn discarded_output_is_drained_through_normal_completion() {
    // Arrange
    let fixture = Fixture::ready();
    fixture.state.flood.store(true, Ordering::SeqCst);
    let PreparedExecution { control, execution } = fixture.prepare(1);
    let mut running = execution.run();
    poll_pending(running.as_mut());
    wait_count(&fixture.state.reading, 200).await;

    // Act
    fixture.state.flood.store(false, Ordering::SeqCst);
    fixture.complete();
    let result = running.await;

    // Assert
    assert_eq!(result.termination, Termination::Completed);
    assert_eq!(result.output.stdout(), &[255]);
    assert!(result.output.truncated());
    assert_eq!(control.cleanup().await, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn a_lost_start_signal_settles_without_preparing() {
    // Arrange
    let fixture = Fixture::ready();
    let (_cancel, cancellation) = watch::channel(false);
    let (start, started) = oneshot::channel();
    let (result, outcome) = oneshot::channel();
    let (settlement, settled) = watch::channel(None);
    let (_retry, retries) = mpsc::channel(1);
    let worker = Worker {
        cancellation,
        clock: Arc::new(TestClock),
        command: command(),
        limits: limits(0),
        policy: policy(),
        process: fixture.backend.bind().expect("inert binding"),
    };

    // Act
    drop(start);
    worker.supervise(started, result, settlement, retries).await;

    // Assert
    assert_eq!(
        outcome.await.expect("outcome").termination,
        Termination::Cancelled
    );
    assert_eq!(*settled.borrow(), Some(Ok(())));
    assert_eq!(fixture.state.preparing.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn preparation_and_start_spend_the_same_deadline_while_cleanup_has_its_own_budget() {
    // Arrange
    let fixture = Fixture::new(
        Mode::Delay(Duration::from_secs(3)),
        Mode::Delay(Duration::from_secs(3)),
        vec![Mode::Stall; 2],
    );
    fixture.event(Event::MainExit(MainExit::Code(7)));
    fixture.event(Event::Eof(Stream::Stdout));
    fixture.event(Event::Eof(Stream::Stderr));
    let PreparedExecution { control, execution } = fixture.prepare(0);
    let before = TestClock.now();

    // Act
    let result = execution.run().await;

    // Assert
    assert_eq!(TestClock.now() - before, Duration::from_secs(12));
    assert_eq!(result.termination, Termination::Deadline);
    assert_eq!(result.main_exit, MainExit::Code(7));
    assert_eq!(result.execution_failure, None);
    assert_eq!(
        result.cleanup_failure,
        Some(ExecutionError::CleanupUnconfirmed)
    );
    assert_eq!(control.cleanup().await, Ok(()));
}

struct TestClock;

#[async_trait]
impl Clock for TestClock {
    fn now(&self) -> Instant {
        tokio::time::Instant::now().into_std()
    }

    async fn wait_until(&self, deadline: Instant) {
        tokio::time::sleep_until(deadline.into()).await;
    }
}

struct Fixture {
    backend: Arc<FakeBackend>,
    input: mpsc::Sender<Input>,
    state: Arc<State>,
}

impl Fixture {
    fn ready() -> Self {
        Self::new(Mode::Ready, Mode::Ready, vec![])
    }

    fn staggered_cleanup() -> Self {
        Self::new(
            Mode::Ready,
            Mode::Ready,
            vec![
                Mode::Fail(ExecutionError::Cleanup),
                Mode::Fail(ExecutionError::Cleanup),
                Mode::Stall,
                Mode::Stall,
                Mode::Delay(Duration::from_millis(100)),
            ],
        )
    }

    fn new(prepare: Mode, start: Mode, cleanup: Vec<Mode>) -> Self {
        let (input, events) = mpsc::channel(8);
        let state = Arc::new(State::default());
        let process = FakeProcess {
            cleanup: cleanup.into(),
            events,
            prepare,
            start,
            state: Arc::clone(&state),
        };

        Self {
            backend: Arc::new(FakeBackend(Mutex::new(Some(process)))),
            input,
            state,
        }
    }

    fn prepare(&self, capture: usize) -> PreparedExecution {
        Supervisor::new(self.backend.clone(), Arc::new(TestClock))
            .expect("supervisor runtime")
            .prepare(command(), policy(), limits(capture))
            .expect("inert binding")
    }

    fn send(&self, input: Input) {
        assert!(self.input.try_send(input).is_ok());
    }

    fn event(&self, event: Event) {
        self.send(Input::Event(event));
    }

    fn output(&self, stream: Stream, bytes: &[u8]) {
        self.send(Input::Bytes(stream, bytes.to_vec()));
    }

    fn complete(&self) {
        for event in completion_events() {
            self.event(event);
        }
    }
}

fn completion_events() -> [Event; 4] {
    [
        Event::MainExit(MainExit::Code(0)),
        Event::Eof(Stream::Stdout),
        Event::Eof(Stream::Stderr),
        Event::Quiescent,
    ]
}

fn command() -> Command {
    Command::new("/runtime/tool".into(), vec![], ".".into()).expect("command")
}

fn policy() -> Policy {
    Policy::new(
        "/workspace".into(),
        vec!["/admin/repo".into()],
        Grants::default(),
    )
    .expect("policy")
}

fn limits(capture: usize) -> Limits {
    Limits::new(TestClock.now(), Duration::from_secs(10), capture).expect("limits")
}

fn poll_pending<T>(mut future: std::pin::Pin<&mut (impl Future<Output = T> + ?Sized)>) {
    assert!(matches!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
}

async fn wait_count(counter: &AtomicUsize, expected: usize) {
    for _ in 0..1000 {
        if counter.load(Ordering::SeqCst) >= expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        counter.load(Ordering::SeqCst) >= expected,
        "worker made no progress"
    );
}

#[derive(Default)]
struct State {
    cleaning: AtomicUsize,
    dropped: AtomicUsize,
    flood: AtomicBool,
    preparing: AtomicUsize,
    reading: AtomicUsize,
    released: AtomicBool,
    starting: AtomicUsize,
}

struct RejectBackend;

impl Backend for RejectBackend {
    fn bind(&self) -> Result<Box<dyn Process>, ExecutionError> {
        Err(ExecutionError::Unsupported)
    }
}

struct FakeBackend(Mutex<Option<FakeProcess>>);

impl Backend for FakeBackend {
    fn bind(&self) -> Result<Box<dyn Process>, ExecutionError> {
        Ok(Box::new(
            self.0
                .try_lock()
                .expect("inert lock")
                .take()
                .expect("single binding"),
        ))
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Ready,
    Delay(Duration),
    Fail(ExecutionError),
    Stall,
}

impl Mode {
    async fn apply(self) -> Result<(), ExecutionError> {
        match self {
            Self::Ready => Ok(()),
            Self::Delay(duration) => {
                tokio::time::sleep(duration).await;

                Ok(())
            }
            Self::Fail(error) => Err(error),
            Self::Stall => pending().await,
        }
    }
}

enum Input {
    Event(Event),
    Bytes(Stream, Vec<u8>),
    Failure,
}

struct FakeProcess {
    cleanup: VecDeque<Mode>,
    events: mpsc::Receiver<Input>,
    prepare: Mode,
    start: Mode,
    state: Arc<State>,
}

#[async_trait]
impl Process for FakeProcess {
    async fn prepare(&mut self, command: &Command, policy: &Policy) -> Result<(), ExecutionError> {
        assert_eq!(command.executable(), Path::new("/runtime/tool"));
        assert_eq!(policy.workspace(), Path::new("/workspace"));
        self.state.preparing.fetch_add(1, Ordering::SeqCst);
        self.prepare.apply().await
    }

    async fn start(&mut self) -> Result<(), ExecutionError> {
        self.state.starting.fetch_add(1, Ordering::SeqCst);
        self.start.apply().await
    }

    async fn next_event(&mut self, buffer: &mut [u8]) -> Result<Event, ExecutionError> {
        self.state.reading.fetch_add(1, Ordering::SeqCst);
        if self.state.flood.load(Ordering::SeqCst) {
            buffer.fill(255);
            return Ok(Event::Output(Stream::Stdout, buffer.len()));
        }
        match self.events.recv().await.expect("fixture retains input") {
            Input::Event(event) => Ok(event),
            Input::Bytes(stream, bytes) => {
                buffer[..bytes.len()].copy_from_slice(&bytes);
                Ok(Event::Output(stream, bytes.len()))
            }
            Input::Failure => Err(ExecutionError::Process),
        }
    }

    async fn cleanup(&mut self) -> Result<(), ExecutionError> {
        self.state.cleaning.fetch_add(1, Ordering::SeqCst);
        self.cleanup
            .pop_front()
            .unwrap_or(Mode::Ready)
            .apply()
            .await?;
        self.state.released.store(true, Ordering::SeqCst);
        Ok(())
    }
}

impl Drop for FakeProcess {
    fn drop(&mut self) {
        self.state.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test(start_paused = true)]
async fn progress_does_not_replace_exit_or_completion_observation() {
    // Arrange
    let fixture = Fixture::ready();
    fixture.event(Event::Progress);
    fixture.complete();
    let PreparedExecution { control, execution } = fixture.prepare(0);

    // Act
    let result = execution.run().await;

    // Assert
    assert_eq!(result.termination, Termination::Completed);
    assert_eq!(result.execution_failure, None);
    assert_eq!(control.cleanup().await, Ok(()));
}
