//! Host process-table sampling behind an injectable boundary.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;

use crate::domain::resource::SessionResources;

/// One validated row in a host process-table snapshot.
#[derive(Clone, Debug)]
pub(crate) struct ProcessSample {
    pub(crate) is_alive: bool,
    pub(crate) parent_pid: u32,
    pub(crate) pid: u32,
    pub(crate) resources: SessionResources,
}

/// Capability for obtaining a single coherent host process-table snapshot.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub(crate) trait ResourceClient: Send + Sync {
    /// Returns `None` when process accounting cannot be read reliably.
    async fn sample(&self) -> Option<Vec<ProcessSample>>;
}

/// Process accounting through the macOS/Linux `ps` interface.
pub(crate) struct RealResourceClient;

impl RealResourceClient {
    /// Rejects failed commands and malformed output instead of showing zeros.
    fn parse_output(output: &std::process::Output) -> Option<Vec<ProcessSample>> {
        if !output.status.success() {
            return None;
        }

        parse_process_table(std::str::from_utf8(&output.stdout).ok()?)
    }
}

#[async_trait]
impl ResourceClient for RealResourceClient {
    async fn sample(&self) -> Option<Vec<ProcessSample>> {
        let mut command = Command::new("ps");
        command
            .args(["-A", "-o", "pid=,ppid=,pcpu=,rss=,stat="])
            .env("LC_ALL", "C")
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(2), command.output())
            .await
            .ok()?
            .ok()?;

        Self::parse_output(&output)
    }
}

/// Parses header-free accounting and process state.
fn parse_process_table(output: &str) -> Option<Vec<ProcessSample>> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let parent_pid = fields.next()?.parse().ok()?;
            let cpu_percent: f64 = fields.next()?.parse().ok()?;
            let resident_memory_kib = fields.next()?.parse().ok()?;
            let state = fields.next()?;
            if !cpu_percent.is_finite() || cpu_percent < 0.0 || fields.next().is_some() {
                return None;
            }

            Some(ProcessSample {
                is_alive: !state.starts_with(['Z', 'X', 'x']),
                parent_pid,
                pid,
                resources: SessionResources {
                    cpu_percent,
                    process_count: 1,
                    resident_memory_kib,
                },
            })
        })
        .collect()
}

/// Totals only the root and descendants present in this snapshot.
/// Returns `None` when the tracked root is absent or exited, even if
/// descendants remain. Exited descendants do not contribute to the totals.
pub(crate) fn process_tree_resources(
    samples: &[ProcessSample],
    root: u32,
) -> Option<SessionResources> {
    let by_pid: HashMap<_, _> = samples
        .iter()
        .filter(|sample| sample.is_alive)
        .map(|sample| (sample.pid, sample))
        .collect();
    by_pid.get(&root)?;
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for sample in by_pid.values() {
        children
            .entry(sample.parent_pid)
            .or_default()
            .push(sample.pid);
    }
    let mut pending = vec![root];
    let mut visited = HashSet::new();
    let mut resources = SessionResources::default();
    while let Some(pid) = pending.pop() {
        if !visited.insert(pid) {
            continue;
        }
        let sample = by_pid.get(&pid)?;
        resources.process_count += 1;
        resources.cpu_percent += sample.resources.cpu_percent;
        resources.resident_memory_kib = resources
            .resident_memory_kib
            .saturating_add(sample.resources.resident_memory_kib);
        if let Some(descendants) = children.get(&pid) {
            pending.extend(descendants);
        }
    }

    Some(resources)
}

#[cfg(test)]
#[path = "resource_test.rs"]
mod tests;
