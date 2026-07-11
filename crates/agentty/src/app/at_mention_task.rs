//! Application-owned registry for pending `@`-mention indexing tasks.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use tokio::task::JoinHandle;

use crate::domain::session::SessionId;

static PENDING_AT_MENTION_LOADS: LazyLock<Mutex<HashMap<SessionId, PendingAtMentionLoad>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct PendingAtMentionLoad {
    handle: JoinHandle<()>,
    request_id: u64,
}

/// Aborts and removes any pending debounced load for one session.
pub(crate) fn clear_pending_load(session_id: &str) {
    if let Ok(mut pending_loads) = PENDING_AT_MENTION_LOADS.lock()
        && let Some(pending_load) = pending_loads.remove(session_id)
    {
        pending_load.handle.abort();
    }
}

/// Stores the latest task and aborts any stale debounced predecessor.
pub(crate) fn track_pending_load(session_id: SessionId, request_id: u64, handle: JoinHandle<()>) {
    if let Ok(mut pending_loads) = PENDING_AT_MENTION_LOADS.lock()
        && let Some(previous_task) =
            pending_loads.insert(session_id, PendingAtMentionLoad { handle, request_id })
    {
        previous_task.handle.abort();
    }
}

/// Clears a task entry when the completing request is still current.
pub(crate) fn finish_pending_load(session_id: &str, request_id: u64) {
    if let Ok(mut pending_loads) = PENDING_AT_MENTION_LOADS.lock()
        && pending_loads
            .get(session_id)
            .is_some_and(|task| task.request_id == request_id)
    {
        pending_loads.remove(session_id);
    }
}
