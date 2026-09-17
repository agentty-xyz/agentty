//! Turn-scoped cancellation and observation of retained persistence ownership.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use thiserror::Error;
use tokio::sync::watch;

use crate::session::{SessionError, TurnOwner, recover_abandoned_owner};
use crate::{ModelError, TurnError, TurnOutcome};

/// A lazy turn future with a separately retainable cancellation control.
///
/// Poll or await this value to start execution. Dropping it requests
/// cancellation; keep its [`Self::control`] to observe persistence settlement.
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
            settlement: Settlement(watch::Sender::new(State::default())),
        };
        let lease = control.settlement.retain();
        let worker_control = control.clone();
        let future = Box::pin(async move {
            if *worker_control.cancellation.borrow() {
                return Err(TurnError::Cancelled.into());
            }
            let worker = make(worker_control.clone());
            let mut task = tokio::spawn(async move {
                let result = worker.await;
                drop(lease);

                result
            });
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

/// Cloneable cancellation and persistence observation for exactly one turn.
///
/// Cancellation stops the waiter promptly. A terminal commit already in
/// progress can still succeed. Keep the Tokio runtime driven until settlement.
/// Neither cancellation nor settlement proves filesystem effects have stopped.
#[derive(Clone)]
pub struct TurnControl {
    pub(crate) settlement: Settlement,
    cancellation: watch::Sender<bool>,
}

impl TurnControl {
    /// Requests cancellation without waiting for storage. Repeated calls and
    /// calls after completion cannot affect another turn.
    pub fn cancel(&self) {
        self.cancellation.send_replace(true);
    }

    /// Waits for execution and its retained persistence owners to settle.
    ///
    /// A failed cleanup returns an error while admission remains protected.
    /// Use [`Self::retry_settlement`] after addressing the storage failure.
    /// This does not wait for detached filesystem effects or remote providers.
    ///
    /// # Errors
    /// Returns the bounded diagnostic from an unsuccessful cleanup attempt.
    pub async fn settled(&self) -> Result<(), SettlementError> {
        let mut receiver = self.settlement.0.subscribe();
        loop {
            let state = receiver.borrow_and_update().clone();
            if let Some((_, error)) = state.failure {
                return Err(error);
            }
            if state.pending == 0 {
                return Ok(());
            }
            // This control retains the sender for the entire wait.
            let _ = receiver.changed().await;
        }
    }

    /// Retries only this turn's failed owner-scoped persistence cleanup.
    /// Does not execute a model or tool, or cancel a successor turn.
    /// If cleanup is still pending, this is a no-op; await [`Self::settled`]
    /// to observe its outcome.
    ///
    /// # Errors
    /// Returns the store failure when cleanup still cannot be acknowledged.
    pub async fn retry_settlement(&self) -> Result<(), SessionError> {
        let failure = self.settlement.0.borrow().failure.clone();
        if let Some((owner, _)) = failure {
            recover_abandoned_owner(&owner).await?;
        }

        Ok(())
    }

    pub(crate) async fn cancelled(&self) {
        let mut receiver = self.cancellation.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }
}

/// A cleanup failure observed independently of the turn's execution result.
#[derive(Clone, Debug, Error)]
#[error("turn persistence cleanup failed: {message}")]
pub struct SettlementError {
    message: String,
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
        let error = SettlementError {
            message: crate::schema_contract::bounded_diagnostic(error),
        };
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
