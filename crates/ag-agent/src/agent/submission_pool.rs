//! Scoped runtime slots for concurrent isolated utility submissions.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::agent::create_app_server_client;
use crate::app_server::AppServerClient;
use crate::model::agent::AgentKind;

/// Retains one process per concurrent submission until its owner closes it.
#[derive(Default)]
pub(super) struct SubmissionPool {
    slots: Mutex<Vec<Arc<SubmissionSlot>>>,
}

impl SubmissionPool {
    /// Borrows an idle provider slot or creates one. CLI providers have no
    /// reusable app-server runtime and return no lease.
    pub(super) fn acquire(
        &self,
        kind: AgentKind,
        client_override: Option<Arc<dyn AppServerClient>>,
    ) -> Option<SubmissionLease> {
        let mut slots = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for slot in slots.iter() {
            if slot.kind == kind && !slot.busy.swap(true, Ordering::AcqRel) {
                return Some(SubmissionLease(Arc::clone(slot)));
            }
        }
        let slot = Arc::new(SubmissionSlot {
            client: create_app_server_client(kind, client_override)?,
            session_id: format!("review-runtime-{}", uuid::Uuid::new_v4()),
            busy: AtomicBool::new(true),
            kind,
        });
        slots.push(Arc::clone(&slot));

        Some(SubmissionLease(slot))
    }

    /// Releases every process once all borrowed submissions have settled.
    pub(super) async fn close(&self) {
        let slots = std::mem::take(
            &mut *self
                .slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for slot in slots {
            slot.client.shutdown_session(slot.session_id.clone()).await;
        }
    }
}

/// Provider runtime identity retained across fresh conversations.
pub(super) struct SubmissionSlot {
    pub(super) client: Arc<dyn AppServerClient>,
    pub(super) session_id: String,
    busy: AtomicBool,
    kind: AgentKind,
}

/// Returns the slot on success, error, or cancellation without detached work.
pub(super) struct SubmissionLease(pub(super) Arc<SubmissionSlot>);

impl Drop for SubmissionLease {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}
