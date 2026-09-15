//! Platform-independent ownership and lifecycle orchestration. Only injected
//! backends can supply process operations; no production backend is available.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot, watch};

use super::contract::{
    Command, Execution, ExecutionControl, ExecutionError, ExecutionResult, Executor, Limits,
    MainExit, Output, Policy, PreparedExecution, Stream, Termination,
};

const READ_BYTES: usize = 4096;
const CLEANUP_ATTEMPTS: usize = 2;
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);

/// Inert, synchronous allocation of the cleanup owner. No processes, writes,
/// blocking work, or background preparation may occur here.
pub(super) trait Backend: Send + Sync {
    fn bind(&self) -> Result<Box<dyn Process>, ExecutionError>;
}

/// All acquired resources must be recorded in `self` before an operation can
/// yield. Dropping any operation must leave cleanup authority in `self`, with
/// no detached acquisition still able to create resources afterward. Methods
/// must yield promptly and never block the runtime thread.
#[async_trait]
pub(super) trait Process: Send {
    /// Enforce the complete policy before `start`, or fail closed.
    async fn prepare(&mut self, command: &Command, policy: &Policy) -> Result<(), ExecutionError>;
    async fn start(&mut self) -> Result<(), ExecutionError>;
    /// Read into the supplied bounded buffer, without an unbounded internal
    /// queue. Output lengths cannot exceed the buffer. EOF is per pipe;
    /// quiescence independently confirms that no descendants can remain or
    /// appear. Deliver the main exit separately, even if it is unavailable.
    async fn next_event(&mut self, buffer: &mut [u8]) -> Result<Event, ExecutionError>;
    /// Idempotently stop/reap descendants and release even partially prepared
    /// resources. Success positively confirms cleanup; a dropped attempt must
    /// leave the owner usable for another bounded attempt. This operation owns
    /// any remaining pipe draining/disposal and cannot depend on a consumer.
    async fn cleanup(&mut self) -> Result<(), ExecutionError>;
}

pub(super) enum Event {
    Output(Stream, usize),
    Eof(Stream),
    MainExit(MainExit),
    Quiescent,
}

/// The same monotonic clock supplies execution and cleanup deadlines.
#[async_trait]
pub(super) trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    async fn wait_until(&self, deadline: Instant);
}

pub(super) struct Supervisor {
    backend: Arc<dyn Backend>,
    clock: Arc<dyn Clock>,
    runtime: Handle,
}

impl Supervisor {
    /// Capture the entered runtime before any backend binding. Preparation can
    /// then run on any thread. The host must keep this runtime alive and driven
    /// until cleanup settles; the handle does not prevent runtime shutdown.
    pub(super) fn new(
        backend: Arc<dyn Backend>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ExecutionError> {
        let runtime = Handle::try_current().map_err(|_| ExecutionError::Supervision)?;

        Ok(Self {
            backend,
            clock,
            runtime,
        })
    }
}

impl Executor for Supervisor {
    fn prepare(
        &self,
        command: Command,
        policy: Policy,
        limits: Limits,
    ) -> Result<PreparedExecution, ExecutionError> {
        let process = self.backend.bind()?;
        let (cancel, cancellation) = watch::channel(false);
        let (start, started) = oneshot::channel();
        let (result, outcome) = oneshot::channel();
        let (settlement, settled) = watch::channel(None);
        let (retry, retries) = mpsc::channel(1);
        let worker = Worker {
            cancellation,
            clock: Arc::clone(&self.clock),
            command,
            limits,
            policy,
            process,
        };
        // Dropping a JoinHandle detaches it. This task owns preparation,
        // execution, drain, and cleanup independently of every caller future.
        self.runtime
            .spawn(worker.supervise(started, result, settlement, retries));

        Ok(PreparedExecution {
            control: Box::new(Control {
                cancel: cancel.clone(),
                retry,
                settled,
            }),
            execution: Box::new(Running {
                cancel,
                limits,
                outcome,
                start: Some(start),
            }),
        })
    }
}

struct Control {
    cancel: watch::Sender<bool>,
    retry: mpsc::Sender<oneshot::Sender<Result<(), ExecutionError>>>,
    settled: watch::Receiver<Option<Result<(), ExecutionError>>>,
}

#[async_trait]
impl ExecutionControl for Control {
    fn cancel(&self) {
        self.cancel.send_replace(true);
    }

    async fn cleanup(&self) -> Result<(), ExecutionError> {
        self.cancel();
        let mut settled = self.settled.clone();
        let previous = *settled.borrow_and_update();
        if previous == Some(Ok(())) {
            return Ok(());
        }
        if previous.is_some() {
            // Replies belong to individual requests: a preceding retry's
            // settlement cannot acknowledge this one. Only callers wait for
            // queue space; the worker never waits for a reply consumer.
            let (reply, outcome) = oneshot::channel();
            if self.retry.send(reply).await.is_ok()
                && let Ok(result) = outcome.await
            {
                return result;
            }

            // Confirmed cleanup closes the worker and settles queued callers
            // too. Unexpected channel closure must remain unconfirmed.
            return self
                .settled
                .borrow()
                .unwrap_or(Err(ExecutionError::CleanupUnconfirmed));
        }
        loop {
            settled
                .changed()
                .await
                .map_err(|_| ExecutionError::CleanupUnconfirmed)?;
            if let Some(result) = *settled.borrow_and_update() {
                return result;
            }
        }
    }
}

struct Running {
    cancel: watch::Sender<bool>,
    limits: Limits,
    outcome: oneshot::Receiver<ExecutionResult>,
    start: Option<oneshot::Sender<()>>,
}

#[async_trait]
impl Execution for Running {
    async fn run(mut self: Box<Self>) -> ExecutionResult {
        if let Some(start) = self.start.take() {
            let _ = start.send(());
        }

        (&mut self.outcome)
            .await
            .unwrap_or_else(|_| ExecutionResult {
                cleanup_failure: Some(ExecutionError::CleanupUnconfirmed),
                execution_failure: Some(ExecutionError::Supervision),
                main_exit: MainExit::Unavailable,
                output: Output::new(self.limits),
                termination: Termination::Failed,
            })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.cancel.send_replace(true);
    }
}

struct Worker {
    cancellation: watch::Receiver<bool>,
    clock: Arc<dyn Clock>,
    command: Command,
    limits: Limits,
    policy: Policy,
    process: Box<dyn Process>,
}

impl Worker {
    async fn supervise(
        mut self,
        started: oneshot::Receiver<()>,
        result: oneshot::Sender<ExecutionResult>,
        settlement: watch::Sender<Option<Result<(), ExecutionError>>>,
        mut retries: mpsc::Receiver<oneshot::Sender<Result<(), ExecutionError>>>,
    ) {
        let mut outcome = ExecutionResult {
            cleanup_failure: None,
            execution_failure: None,
            main_exit: MainExit::Unavailable,
            output: Output::new(self.limits),
            termination: Termination::Completed,
        };
        match interruptible(
            self.clock.as_ref(),
            self.limits,
            &mut self.cancellation,
            started,
        )
        .await
        {
            Ok(Ok(())) => self.execute(&mut outcome).await,
            Ok(Err(_)) => outcome.termination = Termination::Cancelled,
            Err(reason) => outcome.termination = reason,
        }
        let mut cleanup = self.clean().await;
        outcome.cleanup_failure = cleanup.err();
        settlement.send_replace(Some(cleanup));
        let _ = result.send(outcome);
        while cleanup.is_err() {
            let Some(reply) = retries.recv().await else {
                return;
            };
            cleanup = self.clean().await;
            settlement.send_replace(Some(cleanup));
            let _ = reply.send(cleanup);
        }
    }

    async fn execute(&mut self, outcome: &mut ExecutionResult) {
        let prepared = interruptible(
            self.clock.as_ref(),
            self.limits,
            &mut self.cancellation,
            self.process.prepare(&self.command, &self.policy),
        )
        .await;
        if !record_phase(prepared, outcome) {
            return;
        }
        let started = interruptible(
            self.clock.as_ref(),
            self.limits,
            &mut self.cancellation,
            self.process.start(),
        )
        .await;
        if !record_phase(started, outcome) {
            return;
        }
        self.drain(outcome).await;
    }

    async fn drain(&mut self, outcome: &mut ExecutionResult) {
        let mut buffer = [0; READ_BYTES];
        let mut stdout_eof = false;
        let mut stderr_eof = false;
        let mut main_exited = false;
        let mut quiescent = false;
        while !(main_exited && quiescent && stdout_eof && stderr_eof) {
            let event = interruptible(
                self.clock.as_ref(),
                self.limits,
                &mut self.cancellation,
                self.process.next_event(&mut buffer),
            )
            .await;
            match event {
                Ok(Ok(Event::Output(stream, length))) if length <= buffer.len() => {
                    outcome.output.capture(stream, &buffer[..length]);
                }
                Ok(Ok(Event::Eof(Stream::Stdout))) => stdout_eof = true,
                Ok(Ok(Event::Eof(Stream::Stderr))) => stderr_eof = true,
                Ok(Ok(Event::MainExit(exit))) => {
                    outcome.main_exit = exit;
                    main_exited = true;
                }
                Ok(Ok(Event::Quiescent)) => quiescent = true,
                other => {
                    record_phase(
                        other.map(|event| event.and(Err(ExecutionError::Process))),
                        outcome,
                    );
                    return;
                }
            }
            // An injected reader may always be ready, even after capture is
            // exhausted. Yield so cancellation and timers can still progress.
            tokio::task::yield_now().await;
        }
    }

    async fn clean(&mut self) -> Result<(), ExecutionError> {
        for _ in 0..CLEANUP_ATTEMPTS {
            let deadline = self.clock.now() + CLEANUP_TIMEOUT;
            let result = tokio::select! {
                biased;
                () = self.clock.wait_until(deadline) => Err(ExecutionError::CleanupUnconfirmed),
                result = self.process.cleanup() => result,
            };
            if result.is_ok() {
                return Ok(());
            }
        }

        Err(ExecutionError::CleanupUnconfirmed)
    }
}

async fn interruptible<T>(
    clock: &dyn Clock,
    limits: Limits,
    cancellation: &mut watch::Receiver<bool>,
    operation: impl Future<Output = T>,
) -> Result<T, Termination> {
    tokio::select! {
        biased;
        _ = cancellation.wait_for(|cancelled| *cancelled) => Err(Termination::Cancelled),
        () = clock.wait_until(limits.deadline()) => Err(Termination::Deadline),
        result = operation => Ok(result),
    }
}

fn record_phase(
    phase: Result<Result<(), ExecutionError>, Termination>,
    outcome: &mut ExecutionResult,
) -> bool {
    match phase {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            outcome.execution_failure = Some(error);
            outcome.termination = Termination::Failed;

            false
        }
        Err(reason) => {
            outcome.termination = reason;

            false
        }
    }
}

#[cfg(test)]
#[path = "supervisor_test.rs"]
mod tests;
