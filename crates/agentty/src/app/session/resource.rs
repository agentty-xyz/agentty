//! Foreground-owned cache of background Agentty process-tree samples.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;

use crate::domain::resource::SessionResources;
use crate::infra::resource::{self, ProcessSample, ResourceClient};

/// Minimum interval between two host process-table samples.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// Samples the Agentty process and all of its descendants, including every
/// session agent runtime, at most once every two seconds.
pub(super) struct ResourceMonitor {
    /// Latest totals for the Agentty process tree.
    pub(super) resources: Option<SessionResources>,
    client: Arc<dyn ResourceClient>,
    deadline: Option<Instant>,
    pending: Option<JoinHandle<Option<Vec<ProcessSample>>>>,
    root_pid: u32,
}

impl ResourceMonitor {
    /// Creates a monitor for the process tree rooted at `root_pid`.
    pub(super) fn new(client: Arc<dyn ResourceClient>, root_pid: u32) -> Self {
        Self {
            resources: None,
            client,
            deadline: None,
            pending: None,
            root_pid,
        }
    }

    /// Applies a finished sample and schedules the next one without waiting
    /// for host I/O. Failed samples clear the totals so stale values are
    /// never displayed.
    ///
    /// Returns whether the displayed totals changed.
    pub(super) async fn refresh(&mut self, now: Instant) -> bool {
        let previous = self.resources;
        if self.pending.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(pending) = self.pending.take()
        {
            self.resources =
                pending.await.ok().flatten().and_then(|processes| {
                    resource::process_tree_resources(&processes, self.root_pid)
                });
        }
        if self.pending.is_none() && self.deadline.is_none_or(|deadline| now >= deadline) {
            self.deadline = Some(now + SAMPLE_INTERVAL);
            let client = Arc::clone(&self.client);
            self.pending = Some(tokio::spawn(async move { client.sample().await }));
        }

        previous != self.resources
    }
}

impl Drop for ResourceMonitor {
    fn drop(&mut self) {
        if let Some(pending) = &self.pending {
            pending.abort();
        }
    }
}

#[cfg(test)]
#[path = "resource_test.rs"]
mod tests;
