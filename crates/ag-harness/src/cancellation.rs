//! Turn-scoped cancellation and one settlement covering persistence, writes,
//! and commands.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::task::{Context, Poll};

use thiserror::Error;
use tokio::sync::watch;

use crate::effect::Effects;
use crate::event::EventSink;
use crate::session::{SessionError, TurnOwner};
use crate::turn::TurnReport;
use crate::{ModelError, TurnError, TurnOutcome, reservation, telemetry};

/// A lazy turn future with a separately retainable cancellation control.
///
/// Poll or await this value to start execution. Dropping it requests
/// cancellation; keep its [`Self::control`] to observe settlement. The
/// OpenTelemetry context current at the first poll stays attached to the turn,
/// so harness spans nest under the caller's span.
#[must_use = "turns do not start until polled"]
pub struct ControlledTurn<'a, E> {
    control: TurnControl,
    future: Pin<Box<dyn Future<Output = Result<TurnOutcome, E>> + Send + 'a>>,
}

impl<'a, E: From<TurnError> + Send + 'static> ControlledTurn<'a, E> {
    /// Returns a control bound exclusively to this turn.
    pub fn control(&self) -> TurnControl {
        self.control.clone()
    }

    pub(crate) fn new<F, Make>(make: Make) -> Self
    where
        F: Future<Output = Result<TurnOutcome, E>> + Send + 'static,
        Make: FnOnce(TurnControl) -> F + Send + 'a,
    {
        let control = TurnControl {
            cancellation: watch::Sender::new(false),
            effects: Effects::default(),
            events: Arc::default(),
            record: Arc::default(),
            settlement: Settlement(watch::Sender::new(State::default())),
        };
        let lease = control.settlement.retain();
        let effects = control.effects.retain();
        let worker_control = control.clone();
        let future = Box::pin(async move {
            if *worker_control.cancellation.borrow() {
                return Err(TurnError::Cancelled.into());
            }
            let worker = make(worker_control.clone());
            // The spawned worker runs outside the caller's poll, so it carries
            // the caller's OpenTelemetry context for the turn span's parent.
            let mut task = tokio::spawn(telemetry::with_current_context(async move {
                let result = worker.await;
                drop(lease);
                drop(effects);

                result
            }));
            tokio::select! {
                biased;
                result = &mut task => result.map_err(|error| {
                    E::from(TurnError::Model(ModelError::request(error)))
                })?,
                () = worker_control.cancelled() => Err(TurnError::Cancelled.into()),
            }
        });

        Self { control, future }
    }
}

impl<E> Future for ControlledTurn<'_, E> {
    type Output = Result<TurnOutcome, E>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.future.as_mut().poll(context)
    }
}

impl<E> Drop for ControlledTurn<'_, E> {
    fn drop(&mut self) {
        self.control.cancel();
    }
}

/// Cloneable cancellation and settlement observation for one turn.
///
/// Cancellation stops the waiter promptly. A terminal commit already in
/// progress can still succeed. Cancellation alone does not prove effects have
/// stopped; wait for [`Self::settled`] and keep the Tokio runtime driven until
/// it returns.
#[derive(Clone)]
pub struct TurnControl {
    pub(crate) effects: Effects,
    pub(crate) settlement: Settlement,
    cancellation: watch::Sender<bool>,
    /// Event sink of a run that streams its events. The run's outcome
    /// future owns the sink, so dropping the run ends the stream.
    events: Arc<OnceLock<Weak<EventSink>>>,
    record: Arc<Mutex<TurnRecord>>,
}

impl TurnControl {
    /// Snapshots observed command outcomes in invocation order. `None` means no
    /// outcome has been observed, including a still-running invocation. Content
    /// may contain secrets; this is host inspection data, never telemetry.
    pub fn command_outcomes(&self) -> Vec<Option<crate::bash::CommandOutcome>> {
        self.effects.commands().outcomes()
    }

    /// Requests cancellation without waiting for storage. Repeated calls and
    /// calls after completion cannot affect another turn.
    pub fn cancel(&self) {
        self.cancellation.send_replace(true);
    }

    /// Waits until the turn has settled: execution and its persistence
    /// owners, then managed filesystem writes, then command processes and
    /// their outcome recording. After success the turn starts no more writes
    /// or commands, so the host may inspect or commit the workspace.
    ///
    /// Success does not mean writes or commands succeeded, and nothing is
    /// rolled back. It does not fence other processes or stop remote
    /// providers. macOS command cleanup covers only its best-effort process
    /// group; detached descendants may remain.
    ///
    /// # Errors
    /// Returns the first phase that failed. Persistence and command failures
    /// keep admission protected until [`Self::retry_settlement`] succeeds; a
    /// write whose completion is unknown keeps it protected for the lifetime
    /// of this process.
    pub async fn settled(&self) -> Result<(), SettlementError> {
        let mut receiver = self.settlement.0.subscribe();
        loop {
            let state = receiver.borrow_and_update().clone();
            if let Some((_, error)) = state.failure {
                return Err(error);
            }
            if state.pending == 0 {
                break;
            }
            // This control retains the sender for the entire wait.
            let _ = receiver.changed().await;
        }
        self.effects
            .settled()
            .await
            .map_err(|error| SettlementError {
                message: error.to_string(),
                phase: SettlementPhase::Writes,
            })?;

        self.effects
            .commands()
            .settled()
            .await
            .map_err(|error| SettlementError {
                message: error.to_string(),
                phase: SettlementPhase::Commands,
            })
    }

    /// Retries this turn's failed persistence cleanup, then its unresolved
    /// command cleanup and outcome recording. Never executes a model, tool,
    /// or command, or cancels a successor turn. Work still in progress is
    /// left alone; await [`Self::settled`] to observe it.
    ///
    /// # Errors
    /// Returns the phase whose cleanup still cannot be acknowledged.
    pub async fn retry_settlement(&self) -> Result<(), SettlementError> {
        let failure = self.settlement.0.borrow().failure.clone();
        if let Some((owner, _)) = failure {
            reservation::recover_owner(&owner)
                .await
                .map_err(|error| SettlementError::persistence(&error))?;
        }
        let commands = self.effects.commands();
        if commands.retryable() {
            commands.retry().await.map_err(|error| SettlementError {
                message: error.to_string(),
                phase: SettlementPhase::Commands,
            })?;
        }

        Ok(())
    }

    pub(crate) async fn cancelled(&self) {
        let mut receiver = self.cancellation.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }

    /// Streams this turn's run events to `events`; only the first sink
    /// attached to a turn is kept.
    pub(crate) fn attach_events(&self, events: &Arc<EventSink>) {
        let _ = self.events.set(Arc::downgrade(events));
    }

    /// Returns the event sink of a run that streams its events and has not
    /// been dropped.
    pub(crate) fn events(&self) -> Option<Arc<EventSink>> {
        self.events.get().and_then(Weak::upgrade)
    }

    /// Records the durable turn position the turn acquired, which scopes its
    /// write and command journal records.
    pub(crate) fn record_turn_position(&self, turn_position: i64) {
        self.lock_record().turn_position = Some(turn_position);
    }

    /// Records what a turn that ended without an outcome did before ending.
    pub(crate) fn record_report(&self, report: &TurnReport) {
        self.lock_record().report = Some(report.clone());
    }

    /// Takes what the turn recorded for its run outcome.
    pub(crate) fn take_record(&self) -> TurnRecord {
        std::mem::take(&mut *self.lock_record())
    }

    fn lock_record(&self) -> MutexGuard<'_, TurnRecord> {
        self.record.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What a controlled turn recorded for the outcome of its run.
#[derive(Default)]
pub(crate) struct TurnRecord {
    /// Report of a turn that failed, was cancelled, or was interrupted after
    /// preparing a model request.
    pub(crate) report: Option<TurnReport>,
    /// Durable turn position, once the turn was acquired.
    pub(crate) turn_position: Option<i64>,
}

/// A settlement phase that failed, observed independently of the turn's
/// execution result.
#[derive(Clone, Debug, Error)]
#[error("turn {} settlement failed: {message}", .phase.as_str())]
pub struct SettlementError {
    message: String,
    phase: SettlementPhase,
}

impl SettlementError {
    /// Returns the phase that failed.
    pub fn phase(&self) -> SettlementPhase {
        self.phase
    }

    fn persistence(error: &SessionError) -> Self {
        Self {
            message: crate::schema_contract::bounded_diagnostic(error),
            phase: SettlementPhase::Persistence,
        }
    }
}

/// Settlement phase reported by [`SettlementError`], in the order
/// [`TurnControl::settled`] waits for them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SettlementPhase {
    /// Recording the turn's terminal state or interrupting its reservation.
    Persistence,
    /// Managed filesystem replacements and their recorded outcomes.
    Writes,
    /// Command process cleanup and recorded outcomes.
    Commands,
}

impl SettlementPhase {
    /// Returns the stable lowercase phase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Persistence => "persistence",
            Self::Writes => "writes",
            Self::Commands => "commands",
        }
    }
}

#[derive(Clone, Default)]
struct State {
    failure: Option<(TurnOwner, SettlementError)>,
    pending: usize,
}

#[derive(Clone)]
pub(crate) struct Settlement(watch::Sender<State>);

impl Settlement {
    pub(crate) fn retain(&self) -> SettlementLease {
        self.0.send_modify(|state| state.pending += 1);

        SettlementLease(self.clone())
    }

    pub(crate) fn failed(&self, owner: &TurnOwner, error: &SessionError) {
        let error = SettlementError::persistence(error);
        self.0
            .send_modify(|state| state.failure = Some((owner.clone(), error)));
    }

    pub(crate) fn recovered(&self) {
        self.0.send_modify(|state| state.failure = None);
    }
}

pub(crate) struct SettlementLease(Settlement);

impl Drop for SettlementLease {
    fn drop(&mut self) {
        self.0.0.send_modify(|state| state.pending -= 1);
    }
}
