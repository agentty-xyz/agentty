//! Shared accounting for provider turns, including transport and protocol
//! retries.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::OneShotError;

/// A shared limit on underlying provider turns across cloned requests.
///
/// Transports must consume one slot immediately before each provider attempt,
/// including repairs and restart retries. Failed attempts also consume a slot.
#[derive(Clone, Debug)]
pub struct ProviderCallBudget {
    remaining: Arc<AtomicUsize>,
}

impl ProviderCallBudget {
    /// Creates a budget shared by all clones.
    pub fn new(limit: usize) -> Self {
        Self {
            remaining: Arc::new(AtomicUsize::new(limit)),
        }
    }

    /// Checks for exhaustion without charging a turn at an outer boundary.
    ///
    /// # Errors
    /// Returns an input-size reduction error when no slots remain.
    pub fn ensure_available(&self) -> Result<(), OneShotError> {
        if self.remaining.load(Ordering::Relaxed) == 0 {
            return Err(Self::exhausted());
        }

        Ok(())
    }

    /// Charges one underlying provider attempt atomically.
    ///
    /// # Errors
    /// Returns an input-size reduction error before execution when exhausted.
    pub fn consume(&self) -> Result<(), OneShotError> {
        self.remaining
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                remaining.checked_sub(1)
            })
            .map(|_| ())
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
