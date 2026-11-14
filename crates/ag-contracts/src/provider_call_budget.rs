//! Shared accounting for provider turns, including transport and protocol
//! retries.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::OneShotError;

/// Observes every change to one shared provider-call pool, so a host can
/// persist spending across process restarts.
///
/// A charge is reported after a call leaves the pool, including the initial
/// call held by a reservation. A refund is reported before an unused
/// reservation returns its call, so a returned call is never charged again
/// ahead of its refund. Charges minus refunds therefore never exceed the
/// calls spent, and equal them once accounting is idle.
/// Implementations must not block; they run inside budget accounting.
pub trait ProviderCallLedger: Send + Sync {
    /// One call left the shared pool.
    fn charged(&self);

    /// One unused reserved call returned to the shared pool.
    fn refunded(&self);
}

/// A shared limit on underlying provider turns across cloned requests.
///
/// Transports must consume one slot immediately before each provider attempt,
/// including repairs and restart retries. Failed attempts also consume a slot.
#[derive(Clone, Debug)]
pub struct ProviderCallBudget {
    ceiling: Option<Box<ProviderCallBudget>>,
    pool: Arc<SharedPool>,
    reservation: Option<Arc<ReservedCall>>,
}

impl ProviderCallBudget {
    /// Creates a budget shared by all clones.
    pub fn new(limit: usize) -> Self {
        Self::from_pool(SharedPool {
            calls: AtomicUsize::new(limit),
            ledger: None,
        })
    }

    /// Creates a budget whose shared pool reports each charge and refund to
    /// `ledger`. `remaining` is the credit still available, for example after
    /// restoring spending recorded before a restart.
    pub fn with_ledger(remaining: usize, ledger: Arc<dyn ProviderCallLedger>) -> Self {
        Self::from_pool(SharedPool {
            calls: AtomicUsize::new(remaining),
            ledger: Some(ledger),
        })
    }

    /// Returns available shared calls plus this budget's reserved initial call.
    /// Calls reserved by other budgets are unavailable to this budget.
    pub fn remaining(&self) -> usize {
        let remaining = self
            .pool
            .calls
            .load(Ordering::Relaxed)
            .saturating_add(usize::from(
                self.reservation
                    .as_ref()
                    .is_some_and(|call| call.available.load(Ordering::Relaxed)),
            ));

        self.ceiling
            .as_ref()
            .map_or(remaining, |ceiling| remaining.min(ceiling.remaining()))
    }

    /// Adds an enclosing workflow ceiling while preserving the caller's own
    /// limit. Each distinct shared counter charges every provider attempt once.
    #[must_use]
    pub fn with_ceiling(mut self, ceiling: Self) -> Self {
        let mut next = Some(ceiling);
        while let Some(mut counter) = next {
            next = counter.ceiling.take().map(|ceiling| *ceiling);
            self.merge_counter(counter);
        }

        self
    }

    /// Atomically reserves one initial attempt in a child budget.
    ///
    /// The returned budget and its clones redeem that call once through
    /// `consume`; subsequent attempts charge the shared pool normally.
    /// Dropping all child clones before redemption refunds the reservation
    /// to its source. Admission must retain the child through worker delivery.
    ///
    /// # Errors
    /// Returns the same exhaustion error as `consume` when no call is
    /// available.
    pub fn reserve(&self) -> Result<Self, OneShotError> {
        let source = self.take_call()?;

        let mut reserved = Self {
            ceiling: None,
            pool: self.pool.clone(),
            reservation: Some(Arc::new(ReservedCall {
                available: AtomicBool::new(true),
                source,
            })),
        };
        reserved.ceiling = self
            .ceiling
            .as_ref()
            .map(|ceiling| ceiling.reserve().map(Box::new))
            .transpose()?;

        Ok(reserved)
    }

    /// Checks for exhaustion without charging a turn at an outer boundary.
    ///
    /// # Errors
    /// Returns an input-size reduction error when no slots remain.
    pub fn ensure_available(&self) -> Result<(), OneShotError> {
        if self.remaining() == 0 {
            return Err(Self::exhausted());
        }

        Ok(())
    }

    /// Charges one underlying provider attempt atomically.
    /// Redeems a reserved initial call before charging the shared pool.
    ///
    /// # Errors
    /// Returns an input-size reduction error before execution when exhausted.
    /// Failed consumption refunds every debit, including reserved credit.
    pub fn consume(&self) -> Result<(), OneShotError> {
        // Reservation rolls back earlier counters if a later ceiling loses a
        // race. Redeem only after every counter has supplied its credit.
        let reservation = self.reserve()?;
        let mut counter = Some(&reservation);
        while let Some(reserved) = counter {
            reserved.take_call()?;
            counter = reserved.ceiling.as_deref();
        }

        Ok(())
    }

    fn from_pool(pool: SharedPool) -> Self {
        Self {
            ceiling: None,
            pool: Arc::new(pool),
            reservation: None,
        }
    }

    /// Incoming chains are flattened before merging so overlapping tails cannot
    /// charge a counter twice. Retain available reserved credit when present.
    fn merge_counter(&mut self, counter: Self) {
        let mut current = self;
        loop {
            if Arc::ptr_eq(&current.pool, &counter.pool) {
                if let Some(reservation) = counter.reservation
                    && reservation.available.load(Ordering::Relaxed)
                    && !current
                        .reservation
                        .as_ref()
                        .is_some_and(|call| call.available.load(Ordering::Relaxed))
                {
                    current.reservation = Some(reservation);
                }

                return;
            }
            if let Some(ref mut ceiling) = current.ceiling {
                current = ceiling;
            } else {
                current.ceiling = Some(Box::new(counter));

                return;
            }
        }
    }

    fn take_call(&self) -> Result<CallSource, OneShotError> {
        if let Some(call) = &self.reservation
            && call.available.swap(false, Ordering::Relaxed)
        {
            return Ok(CallSource::Reserved(call.clone()));
        }

        self.pool.take()?;

        Ok(CallSource::Shared(self.pool.clone()))
    }

    /// Keep exhaustion terminal for commit assistance as well as transport
    /// retries.
    fn exhausted() -> OneShotError {
        OneShotError::new(
            "Input exceeds the maximum length reduction budget: provider call limit reached; \
             changes are preserved",
        )
    }
}

/// One counter shared by every clone, reservation, and ceiling merge.
struct SharedPool {
    calls: AtomicUsize,
    ledger: Option<Arc<dyn ProviderCallLedger>>,
}

impl SharedPool {
    /// Debits one call and reports it only after the debit succeeded.
    fn take(&self) -> Result<(), OneShotError> {
        self.calls
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |calls| {
                calls.checked_sub(1)
            })
            .map_err(|_| ProviderCallBudget::exhausted())?;
        if let Some(ledger) = &self.ledger {
            ledger.charged();
        }

        Ok(())
    }

    /// Reports the refund before the call returns. Crediting first would let
    /// another owner charge the returned call ahead of its refund, and a
    /// ledger capped at the limit would then drop that charge.
    fn refund(&self) {
        if let Some(ledger) = &self.ledger {
            ledger.refunded();
        }
        self.calls.fetch_add(1, Ordering::Relaxed);
    }
}

impl fmt::Debug for SharedPool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedPool")
            .field("calls", &self.calls)
            .field("has_ledger", &self.ledger.is_some())
            .finish()
    }
}

/// Clones share one redeemable call; only its last owner can refund it.
#[derive(Debug)]
struct ReservedCall {
    available: AtomicBool,
    source: CallSource,
}

impl Drop for ReservedCall {
    fn drop(&mut self) {
        if self.available.load(Ordering::Relaxed) {
            match &self.source {
                CallSource::Reserved(parent) => parent.available.store(true, Ordering::Relaxed),
                CallSource::Shared(pool) => pool.refund(),
            }
        }
    }
}

/// Nested reservations return unused credit to their original owner.
#[derive(Debug)]
enum CallSource {
    Reserved(Arc<ReservedCall>),
    Shared(Arc<SharedPool>),
}

#[cfg(test)]
#[path = "provider_call_budget_test.rs"]
mod tests;
