use std::num::NonZeroUsize;
use std::sync::Arc;

use tokio::sync::{AcquireError, OwnedSemaphorePermit, Semaphore};

/// Shared, first-come admission capacity for session model turns and cleanup.
///
/// Clones share separate turn and cleanup pools. Per-session command ordering
/// remains with the run worker.
#[derive(Clone)]
pub struct SessionAdmission {
    capacity: Arc<Semaphore>,
    cleanup_capacity: Arc<Semaphore>,
}

impl SessionAdmission {
    /// Creates shared turn and cleanup pools with the same positive limit.
    pub fn new(limit: NonZeroUsize) -> Self {
        Self {
            capacity: Arc::new(Semaphore::new(limit.get())),
            cleanup_capacity: Arc::new(Semaphore::new(limit.get())),
        }
    }

    /// Waits for capacity. Dropping the returned permit releases one slot.
    ///
    /// # Errors
    /// Returns an error if admission was closed while waiting.
    pub async fn acquire(&self) -> Result<OwnedSemaphorePermit, AcquireError> {
        Arc::clone(&self.capacity).acquire_owned().await
    }

    /// Waits for a provider cleanup slot, independently of turn admission.
    ///
    /// # Errors
    /// Returns an error if admission was closed while waiting.
    pub async fn acquire_cleanup(&self) -> Result<OwnedSemaphorePermit, AcquireError> {
        Arc::clone(&self.cleanup_capacity).acquire_owned().await
    }

    /// Closes admission and wakes pending callers during shutdown.
    pub fn close(&self) {
        self.capacity.close();
        self.cleanup_capacity.close();
    }
}

#[cfg(test)]
#[path = "admission_test.rs"]
mod tests;
