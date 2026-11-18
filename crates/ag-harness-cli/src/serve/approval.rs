//! Approvals `serve` asks its client for, as `approval/decide` server
//! requests.
//!
//! The harness asks [`Approvals`] about a call after emitting the run's
//! `approval_requested` event. The ask goes to the task forwarding that
//! run's events, which writes the server request only after the event, and
//! registers the decision the client's answer resolves.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ag_harness::event::RunId;
use ag_harness::{ApprovalHandler, ApprovalRequest, Decision};
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use super::protocol::ServerRequest;

/// Feedback for a call no client can be asked about, such as one whose run
/// already ended.
const NO_CLIENT: &str = "no client can approve this call";

/// One call a run waits for a decision on.
pub(crate) struct Ask {
    /// Resolved with the client's decision.
    pub(crate) decision: oneshot::Sender<Decision>,
    pub(crate) request: ApprovalRequest,
}

/// Routes each run's approvals to the task forwarding its events, and the
/// client's answers back to the waiting runs.
#[derive(Clone, Default)]
pub(crate) struct Approvals {
    inner: Arc<Inner>,
}

/// State shared by every clone of [`Approvals`].
#[derive(Default)]
struct Inner {
    next_id: AtomicU64,
    /// Decisions waiting for the client's answer, by server request
    /// identifier.
    pending: Mutex<HashMap<String, oneshot::Sender<Decision>>>,
    /// Asks of each run in flight, read by the task forwarding its events.
    runs: Mutex<HashMap<RunId, mpsc::Sender<Ask>>>,
}

impl Approvals {
    /// Starts routing the approvals of `run` and returns where they arrive.
    pub(crate) fn open(&self, run: RunId) -> mpsc::Receiver<Ask> {
        let (sender, receiver) = mpsc::channel(1);
        lock(&self.inner.runs).insert(run, sender);

        receiver
    }

    /// Stops routing the approvals of `run`, which ended.
    pub(crate) fn close(&self, run: &RunId) {
        lock(&self.inner.runs).remove(run);
    }

    /// Registers `ask` and returns the server request asking the client
    /// about it on behalf of a run in `session`. Decisions whose run
    /// stopped waiting, as a timeout or cancellation does, are dropped.
    pub(crate) fn request(&self, session: String, ask: Ask) -> ServerRequest {
        let id = format!(
            "approval-{}",
            self.inner.next_id.fetch_add(1, Ordering::Relaxed)
        );
        let mut pending = lock(&self.inner.pending);
        pending.retain(|_, decision| !decision.is_closed());
        pending.insert(id.clone(), ask.decision);

        ServerRequest::approval(id, session, ask.request)
    }

    /// Resolves the server request `id` with `decision`; an answer to an
    /// unknown request, or one the run stopped waiting for, is dropped.
    pub(crate) fn answer(&self, id: &Value, decision: Decision) {
        let Some(id) = id.as_str() else {
            return;
        };
        if let Some(waiting) = lock(&self.inner.pending).remove(id) {
            let _ = waiting.send(decision);
        }
    }
}

#[async_trait]
impl ApprovalHandler for Approvals {
    /// Asks the client through the task forwarding the run's events and
    /// waits for its answer; denies a call no client can be asked about.
    async fn decide(&self, request: &ApprovalRequest) -> Decision {
        let route = request
            .run
            .as_ref()
            .and_then(|run| lock(&self.inner.runs).get(run).cloned());
        let (decision, answer) = oneshot::channel();
        let ask = Ask {
            decision,
            request: request.clone(),
        };
        let decided = async {
            route?.send(ask).await.ok()?;

            answer.await.ok()
        };

        decided.await.unwrap_or_else(|| Decision::Deny {
            feedback: NO_CLIENT.to_string(),
        })
    }
}

/// Locks `mutex`, recovering the map of a panicked holder, which every
/// operation leaves consistent.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "approval_test.rs"]
mod tests;
