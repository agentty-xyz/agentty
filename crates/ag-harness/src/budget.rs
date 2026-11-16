//! Budgets shared across runs: a [`BudgetLedger`] reserves an [`Estimate`]
//! before every model request and commits the request's actual usage and
//! cost after it, so parallel runs drawing on one budget stop together.
//!
//! [`SharedBudget`] is the built-in ledger, with optional caps on total
//! micro-USD and total tokens; [`Unlimited`], the default, never refuses.
//! Set a ledger for every run of a harness with
//! [`crate::HarnessBuilder::budget`], or for one run with
//! [`crate::RunRequest::budget`].

use std::fmt;
use std::pin::pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use tokio::sync::Notify;

use crate::limit::LimitKind;
use crate::run::Usage;
use crate::usage::Cost;

/// Shared spending account that admits or refuses each model request.
///
/// The harness calls [`Self::reserve`] before every model request of a
/// run, including context summaries, re-sent invalid outputs, and requests
/// re-sent after a context-window rejection. It reserves once the request
/// holds its model's concurrency permit, if the model has a cap, and sends
/// the request only once the reservation is granted. After sending, it
/// always passes the [`Reservation`] back to [`Self::commit`]: with the
/// request's actual [`Usage`] and its [`Cost`] when the model has prices
/// once the provider answered with usage, and otherwise with a `Usage`
/// whose `unreported_requests` is `1`. The second case covers an answer
/// without usage, a request that failed after it was sent, and one dropped
/// in flight by cancellation or a wall-clock limit, since the provider may
/// have billed any of them. A reservation is released unspent only when it
/// is dropped before its request was sent.
///
/// A refused reservation ends the run with
/// [`crate::ErrorKind::BudgetExhausted`] naming the returned [`LimitKind`],
/// settled like a [`crate::RunLimits`] stop: the run keeps the usage of its
/// completed requests, ends with its `Completed` event, and its session
/// accepts the next run.
#[async_trait]
pub trait BudgetLedger: Send + Sync {
    /// Reserves `estimate` for one model request, waiting if the ledger
    /// chooses to.
    ///
    /// # Errors
    /// Returns the cap the request would exceed; the request is not sent.
    async fn reserve(&self, estimate: Estimate) -> Result<Reservation, LimitKind>;

    /// Settles `reservation` with the request's `actual` usage and its
    /// `cost`, or `None` for a model without prices.
    ///
    /// `actual.unreported_requests` is `1` when the provider reported no
    /// input or output count, or never answered; the counts it lacks are
    /// then zero.
    fn commit(&self, reservation: Reservation, actual: &Usage, cost: Option<Cost>);
}

/// What one model request is expected to use, reserved before it is sent.
///
/// The harness estimates input tokens from the request's estimated weight,
/// calibrated by the provider's earlier usage reports. It reserves output
/// tokens up to the run's [`crate::GenerationParams::max_output_tokens`],
/// or else the cataloged model's [`crate::catalog::ModelSpec::max_output`],
/// or else the output the model's context budget reserves, calibrated like
/// input. A model with prices also gets the cost of those tokens at its
/// input and output prices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Estimate {
    /// Cost of the estimated tokens, or `None` for a model without prices.
    pub cost: Option<Cost>,
    /// Expected input tokens.
    pub input_tokens: u64,
    /// Output tokens reserved for the response.
    pub output_tokens: u64,
}

impl Estimate {
    /// Estimates `input_tokens` and `output_tokens` with no cost.
    pub const fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            cost: None,
            input_tokens,
            output_tokens,
        }
    }

    /// Sets the cost of the estimated tokens.
    #[must_use]
    pub const fn with_cost(mut self, cost: Cost) -> Self {
        self.cost = Some(cost);

        self
    }

    /// Returns the estimated input plus output tokens, saturating.
    pub const fn tokens(self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    /// Returns the estimated cost in micro-USD, zero without a cost.
    const fn micro_usd(self) -> u64 {
        match self.cost {
            Some(cost) => cost.micro_usd,
            None => 0,
        }
    }
}

/// Function that returns a dropped [`Reservation`]'s estimate to its ledger.
type Release = Box<dyn FnOnce(Estimate) + Send>;

/// Budget a [`BudgetLedger`] set aside for one model request.
///
/// It is settled exactly once: the ledger's [`BudgetLedger::commit`]
/// consumes it with [`Self::settle`], and dropping it unsettled runs the
/// release function it was created with, so reserved budget never leaks.
#[must_use = "dropping a reservation releases it"]
pub struct Reservation {
    estimate: Estimate,
    release: Option<Release>,
}

impl Reservation {
    /// Reserves `estimate`; dropping the reservation unsettled calls
    /// `release` with it.
    pub fn new(estimate: Estimate, release: impl FnOnce(Estimate) + Send + 'static) -> Self {
        Self {
            estimate,
            release: Some(Box::new(release)),
        }
    }

    /// Returns the reserved estimate.
    pub fn estimate(&self) -> Estimate {
        self.estimate
    }

    /// Consumes the reservation without releasing it and returns its
    /// estimate, for a ledger recording the actual spend in its place.
    pub fn settle(mut self) -> Estimate {
        self.release = None;

        self.estimate
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release(self.estimate);
        }
    }
}

impl fmt::Debug for Reservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Reservation")
            .field("estimate", &self.estimate)
            .finish_non_exhaustive()
    }
}

/// Ledger that admits every request and records nothing, the default.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unlimited;

#[async_trait]
impl BudgetLedger for Unlimited {
    async fn reserve(&self, estimate: Estimate) -> Result<Reservation, LimitKind> {
        Ok(Reservation::new(estimate, drop))
    }

    fn commit(&self, _reservation: Reservation, _actual: &Usage, _cost: Option<Cost>) {}
}

/// Built-in [`BudgetLedger`] with optional caps on total micro-USD and
/// total input plus output tokens, shared by every run given it.
///
/// Clones share one account: keep a clone to read [`Self::balance`] and
/// hand others, as `Arc<dyn BudgetLedger>`, to harnesses and runs.
///
/// A reservation is admitted while committed spend, open reservations, and
/// its own estimate together stay within every cap. When only open
/// reservations keep it out, it waits for them to be committed or released
/// and checks again, since they may settle below their estimates. It is
/// refused, with [`LimitKind::Cost`] or [`LimitKind::Tokens`], cost checked
/// first, only once committed spend and its estimate alone pass a cap.
/// Committed spend never shrinks, so a refusal is final: the same estimate
/// would be refused again. A commit replaces the reservation's estimate
/// with the actual usage and cost, or with the estimate itself when the
/// provider reported no input or output count. Requests of a model without
/// prices cost nothing against the cost cap; cap their tokens instead.
///
/// Admission is decided atomically under one lock; with equal per-request
/// estimates and equal actual costs, the number of admitted requests is
/// fixed regardless of how parallel runs interleave. Committed spend can
/// exceed a cap only by what requests actually used beyond their
/// estimates: at any moment, committed spend is at most the cap plus the
/// sum, over the committed requests, of `actual - estimate` where positive.
/// A request whose usage never exceeds its estimate, such as one whose
/// output stays within the reserved output and whose input matches the
/// calibrated estimate, therefore never takes the total past the cap. The
/// bound is only as tight as the output reserve: set
/// [`crate::GenerationParams::max_output_tokens`] or
/// [`crate::catalog::ModelSpec::with_max_output`] so estimates cover the
/// output a response can bring.
#[derive(Clone, Debug, Default)]
pub struct SharedBudget {
    account: Arc<Account>,
}

impl SharedBudget {
    /// Creates an uncapped budget; add caps with [`Self::with_max_micro_usd`]
    /// and [`Self::with_max_tokens`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Caps the total cost of committed and reserved requests at
    /// `micro_usd`.
    #[must_use]
    pub fn with_max_micro_usd(self, micro_usd: u64) -> Self {
        self.account.lock().max_micro_usd = Some(micro_usd);

        self
    }

    /// Caps the total input plus output tokens of committed and reserved
    /// requests at `tokens`.
    #[must_use]
    pub fn with_max_tokens(self, tokens: u64) -> Self {
        self.account.lock().max_tokens = Some(tokens);

        self
    }

    /// Returns the committed spend and open reservations now.
    pub fn balance(&self) -> Balance {
        *self.account.lock()
    }
}

#[async_trait]
impl BudgetLedger for SharedBudget {
    async fn reserve(&self, estimate: Estimate) -> Result<Reservation, LimitKind> {
        loop {
            // Registered before the check, so a settlement between the
            // check and the wait still wakes this reservation.
            let mut settled = pin!(self.account.settled.notified());
            settled.as_mut().enable();
            let admission = self.account.lock().admit(estimate);
            match admission {
                Admission::Admitted => break,
                Admission::Refused(limit) => return Err(limit),
                Admission::Waiting => settled.await,
            }
        }
        let account = Arc::clone(&self.account);

        Ok(Reservation::new(estimate, move |estimate| {
            account.settle(estimate, 0, 0);
        }))
    }

    fn commit(&self, reservation: Reservation, actual: &Usage, cost: Option<Cost>) {
        let estimate = reservation.settle();
        let (micro_usd, tokens) = if actual.unreported_requests > 0 {
            (estimate.micro_usd(), estimate.tokens())
        } else {
            (
                cost.map_or(0, |cost| cost.micro_usd),
                actual.input.saturating_add(actual.output),
            )
        };
        self.account.settle(estimate, micro_usd, tokens);
    }
}

/// The balance of a [`SharedBudget`] and the signal waiting reservations
/// watch for settlements.
#[derive(Debug, Default)]
struct Account {
    balance: Mutex<Balance>,
    settled: Notify,
}

impl Account {
    fn lock(&self) -> MutexGuard<'_, Balance> {
        self.balance.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Returns `estimate`'s reservation, spends `micro_usd` and `tokens`,
    /// and wakes the waiting reservations to check again.
    fn settle(&self, estimate: Estimate, micro_usd: u64, tokens: u64) {
        {
            let mut balance = self.lock();
            balance.reserved_micro_usd = balance
                .reserved_micro_usd
                .saturating_sub(estimate.micro_usd());
            balance.reserved_tokens = balance.reserved_tokens.saturating_sub(estimate.tokens());
            balance.spent_micro_usd = balance.spent_micro_usd.saturating_add(micro_usd);
            balance.spent_tokens = balance.spent_tokens.saturating_add(tokens);
        }
        self.settled.notify_waiters();
    }
}

/// Snapshot of a [`SharedBudget`]: its caps, committed spend, and open
/// reservations.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct Balance {
    /// Cap on total micro-USD, if any.
    pub max_micro_usd: Option<u64>,
    /// Cap on total input plus output tokens, if any.
    pub max_tokens: Option<u64>,
    /// Estimated micro-USD of open reservations.
    pub reserved_micro_usd: u64,
    /// Estimated tokens of open reservations.
    pub reserved_tokens: u64,
    /// Micro-USD of committed requests.
    pub spent_micro_usd: u64,
    /// Input plus output tokens of committed requests.
    pub spent_tokens: u64,
}

/// Whether a [`SharedBudget`] admits an estimate now.
enum Admission {
    /// The estimate fits beside committed spend and open reservations.
    Admitted,
    /// The estimate fits beside committed spend alone, so it may fit once
    /// open reservations settle.
    Waiting,
    /// The estimate passes this cap beside committed spend alone.
    Refused(LimitKind),
}

impl Balance {
    /// Reserves `estimate` when it fits every cap beside committed spend
    /// and open reservations.
    fn admit(&mut self, estimate: Estimate) -> Admission {
        let caps = [
            (
                LimitKind::Cost,
                self.max_micro_usd,
                self.spent_micro_usd,
                self.reserved_micro_usd,
                estimate.micro_usd(),
            ),
            (
                LimitKind::Tokens,
                self.max_tokens,
                self.spent_tokens,
                self.reserved_tokens,
                estimate.tokens(),
            ),
        ];
        let mut admission = Admission::Admitted;
        for (limit, max, spent, reserved, more) in caps {
            let Some(max) = max else {
                continue;
            };
            if spent.saturating_add(more) > max {
                return Admission::Refused(limit);
            }
            if spent.saturating_add(reserved).saturating_add(more) > max {
                admission = Admission::Waiting;
            }
        }
        if matches!(admission, Admission::Admitted) {
            self.reserved_micro_usd = self.reserved_micro_usd.saturating_add(estimate.micro_usd());
            self.reserved_tokens = self.reserved_tokens.saturating_add(estimate.tokens());
        }

        admission
    }
}

#[cfg(test)]
#[path = "budget_test.rs"]
mod tests;
