//! Durable provider-call accounting for launched graphs.
//!
//! Each named budget records its charges and refunds in the store as they
//! happen, so a relaunch after restart resumes from the remaining credit.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use ag_contracts::{ProviderCallBudget, ProviderCallLedger};
use tokio::sync::watch;
use tracing::warn;

use crate::app::AppServices;
use crate::infra::db::AppRepositories;

/// Writes one flush attempts before leaving its spending pending.
const RECORD_ATTEMPTS: u32 = 3;

/// Delay before the first retry of a failed write; each later retry waits
/// one base delay longer.
const RECORD_RETRY_BASE_DELAY: Duration = Duration::from_millis(50);

/// Collects spending for one graph budget until its writer persists it.
pub(super) struct GraphBudgetLedger {
    changed: watch::Sender<()>,
    pending_calls: Arc<AtomicI64>,
}

impl GraphBudgetLedger {
    /// Returns a budget with `remaining` credit whose spending a tracked
    /// writer task records for `budget_id`. The writer flushes the last
    /// change and exits after every budget clone is dropped. Until that
    /// flush succeeds, the graph counts as holding unrecorded spending.
    pub(super) fn budget(
        services: &AppServices,
        graph_id: &str,
        budget_id: &str,
        remaining: usize,
    ) -> ProviderCallBudget {
        let (changed, mut receiver) = watch::channel(());
        let pending_calls = Arc::new(AtomicI64::new(0));
        let ledger = Arc::new(Self {
            changed,
            pending_calls: Arc::clone(&pending_calls),
        });
        let repositories = services.db().clone();
        let writers = services.graph_budget_writers.clone();
        let (graph_id, budget_id) = (graph_id.to_string(), budget_id.to_string());
        writers.open(&graph_id);
        services.track_cleanup_task(tokio::spawn(async move {
            loop {
                let is_open = receiver.changed().await.is_ok();
                Self::flush(&repositories, &graph_id, &budget_id, &pending_calls).await;
                if !is_open {
                    break;
                }
            }
            if pending_calls.load(Ordering::Acquire) == 0 {
                writers.close(&graph_id);
            } else {
                warn!(graph_id, budget_id, "Graph spending stayed unrecorded");
            }
        }));

        ProviderCallBudget::with_ledger(remaining, ledger)
    }

    /// Records the pending spending. A failed write keeps its spending
    /// pending and is retried after a growing delay. Spending still
    /// unrecorded after the last attempt waits for the next change or the
    /// final flush, so only a final flush that keeps failing loses it.
    async fn flush(
        repositories: &AppRepositories,
        graph_id: &str,
        budget_id: &str,
        pending_calls: &AtomicI64,
    ) {
        for attempt in 1..=RECORD_ATTEMPTS {
            let calls = pending_calls.swap(0, Ordering::AcqRel);
            if calls == 0 {
                return;
            }
            let Err(error) = repositories
                .graphs()
                .record_budget_spending(graph_id, budget_id, calls)
                .await
            else {
                return;
            };

            pending_calls.fetch_add(calls, Ordering::AcqRel);
            warn!(graph_id, budget_id, calls, attempt, %error, "Failed to record graph spending");
            if attempt < RECORD_ATTEMPTS {
                tokio::time::sleep(RECORD_RETRY_BASE_DELAY * attempt).await;
            }
        }
    }

    fn record(&self, calls: i64) {
        self.pending_calls.fetch_add(calls, Ordering::AcqRel);
        self.changed.send_replace(());
    }
}

impl ProviderCallLedger for GraphBudgetLedger {
    fn charged(&self) {
        self.record(1);
    }

    fn refunded(&self) {
        self.record(-1);
    }
}

/// Counts, per graph, the budget writers that have not recorded all of their
/// spending. A writer leaves only after its final flush succeeds, so the
/// store may not know every call a counted graph spent.
#[derive(Clone, Default)]
pub(super) struct GraphBudgetWriters(Arc<Mutex<HashMap<String, usize>>>);

impl GraphBudgetWriters {
    /// Whether budgets rebuilt from the store could hand back calls an
    /// earlier pool of the graph already spent.
    pub(super) fn has_unrecorded_spending(&self, graph_id: &str) -> bool {
        self.writers().contains_key(graph_id)
    }

    fn open(&self, graph_id: &str) {
        *self.writers().entry(graph_id.to_string()).or_default() += 1;
    }

    fn close(&self, graph_id: &str) {
        let mut writers = self.writers();
        let open_writers = writers
            .get(graph_id)
            .map_or(0, |open_writers| open_writers.saturating_sub(1));
        if open_writers == 0 {
            writers.remove(graph_id);
        } else {
            writers.insert(graph_id.to_string(), open_writers);
        }
    }

    fn writers(&self) -> MutexGuard<'_, HashMap<String, usize>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
#[path = "graph_budget_test.rs"]
mod tests;
