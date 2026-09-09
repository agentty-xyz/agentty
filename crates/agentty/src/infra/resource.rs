//! Host process-table sampling behind an injectable boundary.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use sysinfo::Components;
use tokio::process::Command;

use crate::domain::resource::SessionResources;
use crate::infra::process_identity::ProcessIdentity;

/// Pins temperature for deterministic feature recordings; `--` pins
/// unavailable.
#[cfg(debug_assertions)]
const CPU_TEMPERATURE_ENV_VAR: &str = "AGENTTY_CPU_TEMPERATURE_CELSIUS";

/// One validated row in a host process-table snapshot.
#[derive(Clone, Debug)]
pub(crate) struct ProcessSample {
    pub(crate) host_cpu_temperature_celsius: Option<f32>,
    pub(crate) identity: Option<ProcessIdentity>,
    pub(crate) is_alive: bool,
    pub(crate) parent_pid: u32,
    pub(crate) pid: u32,
    pub(crate) resources: SessionResources,
}

/// Capability for obtaining a single coherent host process table.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub(crate) trait ResourceClient: Send + Sync {
    /// Returns `None` when process accounting cannot be read reliably.
    async fn sample(&self, roots: Vec<u32>) -> Option<Vec<ProcessSample>>;
}

/// Process accounting through the macOS/Linux `ps` interface.
#[derive(Default)]
pub(crate) struct RealResourceClient {
    temperature: Mutex<TemperatureCache>,
}

impl RealResourceClient {
    /// Reads only tracked roots, off the async executor, before or after `ps`.
    async fn identities(roots: Vec<u32>) -> Option<HashMap<u32, ProcessIdentity>> {
        tokio::task::spawn_blocking(move || {
            roots
                .into_iter()
                .filter_map(|pid| ProcessIdentity::read(pid).map(|identity| (pid, identity)))
                .collect()
        })
        .await
        .ok()
    }

    /// Polls the independent sensor worker without waiting for host I/O.
    fn host_cpu_temperature(&self) -> Option<f32> {
        let read = || {
            self.temperature.lock().ok()?.sample(Instant::now(), || {
                let components = Components::new_with_refreshed_list();

                hottest_cpu_temperature(
                    components
                        .iter()
                        .map(|sensor| (sensor.label(), sensor.temperature())),
                )
            })
        };

        #[cfg(debug_assertions)]
        {
            let pinned = std::env::var(CPU_TEMPERATURE_ENV_VAR).ok();

            Self::temperature_with_override(pinned.as_deref(), read)
        }
        #[cfg(not(debug_assertions))]
        read()
    }

    /// Invalid overrides fall back to live sensors, as with the clock fixture.
    #[cfg(debug_assertions)]
    fn temperature_with_override(
        pinned: Option<&str>,
        read: impl FnOnce() -> Option<f32>,
    ) -> Option<f32> {
        if pinned == Some("--") {
            return None;
        }

        pinned
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|temperature| temperature.is_finite() && *temperature > 0.0)
            .or_else(read)
    }

    /// Rejects failed commands and malformed output instead of showing zeros.
    fn parse_output(output: &std::process::Output) -> Option<Vec<ProcessSample>> {
        if !output.status.success() {
            return None;
        }

        parse_process_table(std::str::from_utf8(&output.stdout).ok()?)
    }

    /// Binds accounting only to roots that remained the same process across
    /// the host snapshot. Missing identities never fall back to numeric PIDs.
    fn bind_identities(
        samples: &mut [ProcessSample],
        before: &HashMap<u32, ProcessIdentity>,
        after: &HashMap<u32, ProcessIdentity>,
    ) {
        for sample in samples {
            sample.identity = before
                .get(&sample.pid)
                .filter(|identity| after.get(&sample.pid) == Some(identity))
                .copied();
        }
    }
}

#[async_trait]
impl ResourceClient for RealResourceClient {
    async fn sample(&self, roots: Vec<u32>) -> Option<Vec<ProcessSample>> {
        let before = Self::identities(roots.clone()).await?;
        let mut command = Command::new("ps");
        command
            .args(["-A", "-o", "pid=,ppid=,pcpu=,rss=,stat="])
            .env("LC_ALL", "C")
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(2), command.output())
            .await
            .ok()?
            .ok()?;
        let mut samples = Self::parse_output(&output)?;
        let after = Self::identities(roots).await?;
        Self::bind_identities(&mut samples, &before, &after);
        let temperature = self.host_cpu_temperature();
        for sample in &mut samples {
            sample.host_cpu_temperature_celsius = temperature;
        }

        Some(samples)
    }
}

/// Keeps at most one sensor worker in flight, refreshing after ten seconds.
/// Readings expire after twenty seconds so a stalled backend cannot freeze
/// process accounting or leave an old temperature displayed indefinitely.
#[derive(Default)]
struct TemperatureCache {
    last_started: Option<Instant>,
    pending: Option<JoinHandle<Option<f32>>>,
    sample: Option<(Instant, Option<f32>)>,
}

impl TemperatureCache {
    fn sample(
        &mut self,
        now: Instant,
        read: impl FnOnce() -> Option<f32> + Send + 'static,
    ) -> Option<f32> {
        if let Some(pending) = &self.pending {
            if !pending.is_finished() {
                return self.cached_temperature(now);
            }

            let pending = self.pending.take()?;
            // Age results from the start of the read, including time spent
            // stalled or waiting for the next process sample to collect them.
            self.sample = Some((self.last_started?, pending.join().ok().flatten()));
        }
        if let Some((sampled_at, temperature)) = self.sample
            && now.saturating_duration_since(sampled_at) < Duration::from_secs(10)
        {
            return temperature;
        }

        if self
            .last_started
            .is_some_and(|started| now.saturating_duration_since(started) < Duration::from_secs(10))
        {
            return self.cached_temperature(now);
        }

        // Also throttle retries if the OS cannot create a worker. Native sensor
        // calls cannot be cancelled; retaining the handle prevents overlapping
        // reads. A dedicated thread does not hold up Tokio runtime shutdown.
        self.last_started = Some(now);
        self.pending = thread::Builder::new()
            .name("agentty-temperature".to_string())
            .spawn(read)
            .ok();

        self.cached_temperature(now)
    }

    fn cached_temperature(&self, now: Instant) -> Option<f32> {
        self.sample
            .filter(|(sampled_at, _)| {
                now.saturating_duration_since(*sampled_at) < Duration::from_secs(20)
            })
            .and_then(|(_, temperature)| temperature)
    }
}

/// Selects only recognizable CPU sensors, excluding GPU, disk, and invalid
/// readings.
fn hottest_cpu_temperature<'a>(
    sensors: impl IntoIterator<Item = (&'a str, Option<f32>)>,
) -> Option<f32> {
    sensors
        .into_iter()
        .filter(|(label, _)| {
            let label = label.to_ascii_lowercase();

            [
                "cpu", "coretemp", "k10temp", "zenpower", "pacc mtr", "eacc mtr",
            ]
            .iter()
            .any(|name| label.contains(name))
                // M5 exposes numbered die probes instead of pACC/eACC labels.
                || label.strip_prefix("pmu tdie").is_some_and(|index| {
                    !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
                })
        })
        .filter_map(|(_, temperature)| temperature)
        .filter(|temperature| temperature.is_finite() && *temperature > 0.0)
        .reduce(f32::max)
}

/// Parses header-free accounting and state; native identity is attached later.
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
                host_cpu_temperature_celsius: None,
                identity: None,
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
