use std::sync::Arc;
use std::time::{Duration, Instant};

use super::ResourceMonitor;
use crate::domain::resource::SessionResources;
use crate::infra::resource::{MockResourceClient, ProcessSample};

/// PID of the tracked Agentty root in monitor tests.
const ROOT_PID: u32 = 10;

async fn finish_sample(monitor: &ResourceMonitor) {
    while !monitor.pending.as_ref().expect("sample task").is_finished() {
        tokio::task::yield_now().await;
    }
}

/// Starts one sample at `now`, waits for it, and applies it.
async fn sample_at(monitor: &mut ResourceMonitor, now: Instant) -> bool {
    monitor.refresh(now).await;
    finish_sample(monitor).await;

    monitor.refresh(now).await
}

fn process_sample(pid: u32, parent_pid: u32) -> ProcessSample {
    ProcessSample {
        is_alive: true,
        parent_pid,
        pid,
        resources: SessionResources {
            cpu_percent: 12.5,
            process_count: 1,
            resident_memory_kib: 2048,
        },
    }
}

#[tokio::test]
async fn totals_include_agentty_descendants() {
    // Arrange
    let mut client = MockResourceClient::new();
    client.expect_sample().times(1).returning(|| {
        Some(vec![
            process_sample(ROOT_PID, 1),
            process_sample(11, ROOT_PID),
            process_sample(12, 11),
            process_sample(99, 1),
        ])
    });
    let mut monitor = ResourceMonitor::new(Arc::new(client), ROOT_PID);

    // Act
    let changed = sample_at(&mut monitor, Instant::now()).await;

    // Assert
    assert!(changed);
    assert_eq!(
        monitor.resources,
        Some(SessionResources {
            cpu_percent: 37.5,
            process_count: 3,
            resident_memory_kib: 6144,
        })
    );
}

#[tokio::test]
async fn sampling_is_throttled_until_the_interval_elapses() {
    // Arrange
    let mut client = MockResourceClient::new();
    client
        .expect_sample()
        .times(1)
        .returning(|| Some(vec![process_sample(ROOT_PID, 1)]));
    let mut monitor = ResourceMonitor::new(Arc::new(client), ROOT_PID);
    let now = Instant::now();

    // Act
    let first_refresh_changed = monitor.refresh(now).await;
    finish_sample(&monitor).await;
    let applied_changed = monitor.refresh(now).await;
    let throttled_changed = monitor.refresh(now + Duration::from_secs(1)).await;

    // Assert
    assert!(!first_refresh_changed);
    assert!(applied_changed);
    assert!(!throttled_changed);
    assert!(monitor.pending.is_none());
    assert_eq!(
        monitor.resources.map(|totals| totals.process_count),
        Some(1)
    );
}

#[tokio::test]
async fn failed_or_rootless_samples_clear_previous_values() {
    // Arrange
    let mut client = MockResourceClient::new();
    let mut snapshots = vec![
        Some(vec![process_sample(ROOT_PID, 1)]),
        None,
        Some(vec![process_sample(ROOT_PID, 1)]),
        Some(vec![process_sample(20, ROOT_PID)]),
    ]
    .into_iter();
    client
        .expect_sample()
        .times(4)
        .returning(move || snapshots.next().expect("expected snapshot"));
    let mut monitor = ResourceMonitor::new(Arc::new(client), ROOT_PID);
    let now = Instant::now();

    // Act / Assert
    for (index, is_available) in [true, false, true, false].into_iter().enumerate() {
        let sampled_at = now + Duration::from_secs(index as u64 * 2);
        assert!(sample_at(&mut monitor, sampled_at).await);
        assert_eq!(monitor.resources.is_some(), is_available);
    }
}

#[tokio::test]
async fn dropping_monitor_aborts_in_flight_sampling() {
    // Arrange
    let mut monitor = ResourceMonitor::new(Arc::new(MockResourceClient::new()), ROOT_PID);
    let pending =
        tokio::spawn(async { std::future::pending::<Option<Vec<ProcessSample>>>().await });
    let abort = pending.abort_handle();
    monitor.pending = Some(pending);

    // Act
    drop(monitor);
    tokio::task::yield_now().await;

    // Assert
    assert!(abort.is_finished());
}
