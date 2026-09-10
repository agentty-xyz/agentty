//! Foreground-owned cache of background process-accounting samples.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;

use crate::domain::resource::SessionResources;
use crate::domain::session::SessionId;
use crate::infra::process_identity::ProcessIdentity;
use crate::infra::resource::{self, ProcessSample, ResourceClient};

/// Samples all tracked session roots together, at most once every two seconds.
pub(super) struct ResourceMonitor {
    pub(super) temperatures: HashMap<SessionId, f32>,
    pub(super) values: HashMap<SessionId, SessionResources>,
    client: Arc<dyn ResourceClient>,
    deadline: Option<Instant>,
    /// First observed native identity; `None` permanently invalidates this root
    /// until its PID is removed or replaced in the session handles.
    identities: HashMap<SessionId, Option<ProcessIdentity>>,
    pending: Option<JoinHandle<Option<Vec<ProcessSample>>>>,
    requested_roots: HashMap<SessionId, u32>,
    sampled_roots: HashMap<SessionId, u32>,
}

impl ResourceMonitor {
    pub(super) fn new(client: Arc<dyn ResourceClient>) -> Self {
        Self {
            temperatures: HashMap::new(),
            values: HashMap::new(),
            client,
            deadline: None,
            pending: None,
            requested_roots: HashMap::new(),
            sampled_roots: HashMap::new(),
            identities: HashMap::new(),
        }
    }

    /// Reduces finished samples and schedules work without waiting for host
    /// I/O. Replaced or removed roots invalidate their cached values
    /// immediately. Missing, exited, or reused roots stay unavailable until
    /// the tracked runtime changes.
    pub(super) async fn refresh(&mut self, roots: HashMap<SessionId, u32>, now: Instant) -> bool {
        let previous = self.values.clone();
        let previous_temperatures = self.temperatures.clone();
        self.values.retain(|id, _| {
            roots
                .get(id)
                .is_some_and(|pid| self.sampled_roots.get(id) == Some(pid))
        });
        self.temperatures
            .retain(|id, _| self.values.contains_key(id));
        self.identities.retain(|id, _| {
            roots
                .get(id)
                .is_some_and(|pid| self.sampled_roots.get(id) == Some(pid))
        });
        self.sampled_roots.retain(|id, _| roots.contains_key(id));
        if self.pending.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(pending) = self.pending.take()
        {
            self.values.clear();
            self.temperatures.clear();
            if let Ok(Some(samples)) = pending.await {
                self.apply_samples(&roots, &samples);
            }
            self.sampled_roots.clone_from(&self.requested_roots);
        }
        if self.pending.is_none()
            && !roots.is_empty()
            && self.deadline.is_none_or(|deadline| now >= deadline)
        {
            let pids = roots.values().copied().collect();
            self.requested_roots = roots;
            self.deadline = Some(now + Duration::from_secs(2));
            let client = Arc::clone(&self.client);
            self.pending = Some(tokio::spawn(async move { client.sample(pids).await }));
        }

        previous != self.values || previous_temperatures != self.temperatures
    }

    /// Applies totals only for unchanged tracked roots with matching live
    /// process identities, permanently invalidating roots that no longer match.
    fn apply_samples(&mut self, roots: &HashMap<SessionId, u32>, samples: &[ProcessSample]) {
        for (id, pid) in &self.requested_roots {
            if roots.get(id) != Some(pid) {
                continue;
            }
            let root = samples.iter().find(|sample| sample.pid == *pid);
            let identity = root
                .filter(|sample| sample.is_alive)
                .and_then(|sample| sample.identity);
            let expected = self.identities.entry(id.clone()).or_insert(identity);
            if identity.is_some()
                && *expected == identity
                && let Some(resources) = resource::process_tree_resources(samples, *pid)
            {
                self.values.insert(id.clone(), resources);
                if let Some(temperature) =
                    root.and_then(|sample| sample.host_cpu_temperature_celsius)
                {
                    self.temperatures.insert(id.clone(), temperature);
                }
            } else {
                *expected = None;
            }
        }
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
