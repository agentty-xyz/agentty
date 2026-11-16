//! Supersession and decode admission for background diff preview loads.
//!
//! Every new preview load supersedes older ones, so work for a selection the
//! user already left stops before its expensive image decode. Decodes are
//! admitted one at a time to bound peak decoder memory.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Shared generation counter and decode gate for diff preview loads.
#[derive(Clone)]
pub(crate) struct DiffPreviewLoads {
    decode_permits: Arc<Semaphore>,
    latest_generation: Arc<AtomicU64>,
}

impl DiffPreviewLoads {
    /// Supersedes outstanding loads and returns a ticket for a new one.
    pub(crate) fn begin(&self) -> DiffPreviewLoadTicket {
        let generation = self.next_generation();

        DiffPreviewLoadTicket {
            decode_permits: Arc::clone(&self.decode_permits),
            generation,
            latest_generation: Arc::clone(&self.latest_generation),
        }
    }

    /// Supersedes outstanding loads without starting a new one.
    pub(crate) fn supersede(&self) {
        self.next_generation();
    }

    /// Advances and returns the generation that now counts as latest.
    fn next_generation(&self) -> u64 {
        self.latest_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }
}

impl Default for DiffPreviewLoads {
    fn default() -> Self {
        Self {
            decode_permits: Arc::new(Semaphore::new(1)),
            latest_generation: Arc::default(),
        }
    }
}

/// One background preview load that may be superseded by a newer one.
pub(crate) struct DiffPreviewLoadTicket {
    decode_permits: Arc<Semaphore>,
    generation: u64,
    latest_generation: Arc<AtomicU64>,
}

impl DiffPreviewLoadTicket {
    /// Waits for the decode gate and returns its permit while this load is
    /// still the latest, or `None` once a newer load superseded it.
    ///
    /// The gate is never closed, so acquiring a permit only waits.
    pub(crate) async fn admit_decode(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.decode_permits)
            .acquire_owned()
            .await
            .ok()
            .filter(|_| self.is_current())
    }

    /// Returns whether no newer load has started since this one.
    fn is_current(&self) -> bool {
        self.latest_generation.load(Ordering::Acquire) == self.generation
    }
}

#[cfg(test)]
#[path = "diff_preview_load_test.rs"]
mod tests;
