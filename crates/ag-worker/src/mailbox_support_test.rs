use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use tokio::sync::{Notify, mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::SessionWorkerHandle;

/// Injects a mailbox for host tests that inspect or reject queued commands.
pub fn test_session_worker_handle<C>(
    queued_work_sequence: Arc<AtomicU64>,
    sender: mpsc::UnboundedSender<C>,
    wakeup: Arc<Notify>,
) -> SessionWorkerHandle<C> {
    SessionWorkerHandle {
        admission: Arc::default(),
        completion: watch::channel(()).1,
        execution: Arc::default(),
        queued_work_sequence,
        sender,
        stop: CancellationToken::new(),
        wakeup,
    }
}
