//! Shared accounting for provider turns, including transport and protocol
//! retries.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::OneShotError;

/// A shared limit on underlying provider turns across cloned requests.
///
/// Transports must consume one slot immediately before each provider attempt,
/// including repairs and restart retries. Failed attempts also consume a slot.
#[derive(Clone, Debug)]
pub struct ProviderCallBudget {
    ceiling: Option<Box<ProviderCallBudget>>,
    remaining: Arc<AtomicUsize>,
    reservation: Option<Arc<ReservedCall>>,
}

impl ProviderCallBudget {
    /// Creates a budget shared by all clones.
    pub fn new(limit: usize) -> Self {
        Self {
            ceiling: None,
            remaining: Arc::new(AtomicUsize::new(limit)),
            reservation: None,
        }
    }

    /// Returns available shared calls plus this budget's reserved initial call.
    /// Calls reserved by other budgets are unavailable to this budget.
    pub fn remaining(&self) -> usize {
        let remaining = self
            .remaining
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
            remaining: self.remaining.clone(),
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

    /// Incoming chains are flattened before merging so overlapping tails cannot
    /// charge a counter twice. Retain available reserved credit when present.
    fn merge_counter(&mut self, counter: Self) {
        let mut current = self;
        loop {
            if Arc::ptr_eq(&current.remaining, &counter.remaining) {
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

        self.remaining
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                remaining.checked_sub(1)
            })
            .map(|_| CallSource::Shared(self.remaining.clone()))
            .map_err(|_| Self::exhausted())
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
                CallSource::Shared(remaining) => {
                    remaining.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

/// Nested reservations return unused credit to their original owner.
#[derive(Debug)]
enum CallSource {
    Reserved(Arc<ReservedCall>),
    Shared(Arc<AtomicUsize>),
}

#[cfg(test)]
#[path = "provider_call_budget_test.rs"]
mod tests;
