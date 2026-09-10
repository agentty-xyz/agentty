use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::ResourceMonitor;
use crate::domain::resource::SessionResources;
use crate::domain::session::SessionId;
use crate::infra::process_identity::ProcessIdentity;
use crate::infra::resource::{MockResourceClient, ProcessSample};

async fn finish_sample(monitor: &ResourceMonitor) {
    while !monitor.pending.as_ref().expect("sample task").is_finished() {
        tokio::task::yield_now().await;
    }
}

fn process_sample(pid: u32, identity: u128, is_alive: bool) -> ProcessSample {
    ProcessSample {
        host_cpu_temperature_celsius: Some(64.5),
        is_alive,
        parent_pid: 1,
        pid,
        resources: SessionResources {
            cpu_percent: 12.5,
            process_count: 1,
            resident_memory_kib: 2048,
        },
        identity: Some(ProcessIdentity(identity)),
    }
}

#[tokio::test]
async fn temperature_sidecar_updates_without_changing_process_totals() {
    // Arrange
    let mut client = MockResourceClient::new();
    let mut temperatures = [Some(64.5), Some(65.0), None].into_iter();
    client.expect_sample().times(3).returning(move |_| {
        let mut root = process_sample(10, 1, true);
        root.host_cpu_temperature_celsius = temperatures.next().expect("sample");
        let mut child = process_sample(11, 2, true);
        child.parent_pid = 10;
        child.host_cpu_temperature_celsius = Some(99.0);
        Some(vec![root, child])
    });
    let mut monitor = ResourceMonitor::new(Arc::new(client));
    let roots = HashMap::from([(SessionId::from("session"), 10)]);
    let now = Instant::now();
    let totals = SessionResources {
        cpu_percent: 25.0,
        process_count: 2,
        resident_memory_kib: 4096,
    };

    // Act / Assert
    for (offset, expected) in [Some(64.5), Some(65.0), None].into_iter().enumerate() {
        let sampled_at = now + Duration::from_secs(offset as u64 * 2);
        monitor.refresh(roots.clone(), sampled_at).await;
        finish_sample(&monitor).await;
        assert!(monitor.refresh(roots.clone(), sampled_at).await);
        assert_eq!(monitor.values["session"], totals);
        assert_eq!(monitor.temperatures.get("session").copied(), expected);
    }
}

#[tokio::test]
async fn idle_exit_or_same_second_pid_reuse_invalidates_root_until_tracking_changes() {
    // Arrange
    for invalid_root in [
        Vec::new(),
        vec![process_sample(10, 1_000_001, false)],
        vec![process_sample(10, 1_000_002, true)],
        vec![ProcessSample {
            host_cpu_temperature_celsius: None,
            identity: None,
            ..process_sample(10, 1_000_001, true)
        }],
    ] {
        let mut client = MockResourceClient::new();
        let mut snapshots = vec![
            Some(vec![process_sample(10, 1_000_001, true)]),
            None,
            Some(invalid_root),
            Some(vec![process_sample(10, 1_000_001, true)]),
            Some(vec![process_sample(10, 2_000_001, true)]),
            Some(vec![process_sample(20, 3_000_001, true)]),
        ]
        .into_iter();
        client
            .expect_sample()
            .times(6)
            .returning(move |_| snapshots.next().expect("expected snapshot"));
        let mut monitor = ResourceMonitor::new(Arc::new(client));
        let roots = HashMap::from([(SessionId::from("session"), 10)]);
        let now = Instant::now();

        // Act / Assert
        for (index, available) in [true, false, false, false].into_iter().enumerate() {
            let sample_time = now + Duration::from_secs(index as u64 * 2);
            monitor.refresh(roots.clone(), sample_time).await;
            finish_sample(&monitor).await;
            monitor.refresh(roots.clone(), sample_time).await;
            assert_eq!(monitor.values.contains_key("session"), available);
            assert_eq!(monitor.temperatures.contains_key("session"), available);
        }
        monitor
            .refresh(HashMap::new(), now + Duration::from_secs(7))
            .await;
        monitor
            .refresh(roots.clone(), now + Duration::from_secs(8))
            .await;
        finish_sample(&monitor).await;
        monitor.refresh(roots, now + Duration::from_secs(8)).await;
        assert_eq!(monitor.values["session"].process_count, 1);

        let replacement = HashMap::from([(SessionId::from("session"), 20)]);
        monitor
            .refresh(replacement.clone(), now + Duration::from_secs(10))
            .await;
        assert!(monitor.values.is_empty());
        assert!(monitor.temperatures.is_empty());
        finish_sample(&monitor).await;
        monitor
            .refresh(replacement, now + Duration::from_secs(10))
            .await;
        assert_eq!(monitor.values["session"].process_count, 1);
    }
}

#[tokio::test]
async fn root_already_exited_at_first_sample_never_attaches_to_recycled_pid() {
    // Arrange
    let mut client = MockResourceClient::new();
    let mut snapshots = vec![
        vec![process_sample(10, 1_000_001, false)],
        vec![process_sample(10, 1_000_002, true)],
    ]
    .into_iter();
    client
        .expect_sample()
        .times(2)
        .returning(move |_| snapshots.next());
    let mut monitor = ResourceMonitor::new(Arc::new(client));
    let roots = HashMap::from([(SessionId::from("session"), 10)]);
    let now = Instant::now();

    // Act / Assert
    for offset in [0, 2] {
        let sample_time = now + Duration::from_secs(offset);
        monitor.refresh(roots.clone(), sample_time).await;
        finish_sample(&monitor).await;
        monitor.refresh(roots.clone(), sample_time).await;
        assert!(monitor.values.is_empty());
        assert!(monitor.temperatures.is_empty());
    }
}

#[tokio::test]
async fn dropping_monitor_aborts_in_flight_sampling() {
    // Arrange
    let mut monitor = ResourceMonitor::new(Arc::new(MockResourceClient::new()));
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

#[tokio::test]
async fn sampling_is_throttled_and_clears_exited_or_replaced_sessions() {
    // Arrange
    let mut client = MockResourceClient::new();
    client.expect_sample().times(1).returning(|_| {
        Some(vec![ProcessSample {
            host_cpu_temperature_celsius: None,
            is_alive: true,
            identity: Some(ProcessIdentity(1_000_001)),
            parent_pid: 1,
            pid: 10,
            resources: SessionResources {
                process_count: 1,
                cpu_percent: 12.5,
                resident_memory_kib: 2048,
            },
        }])
    });
    let mut monitor = ResourceMonitor::new(Arc::new(client));
    let roots = HashMap::from([(SessionId::from("session"), 10)]);
    let now = Instant::now();

    // Act
    assert!(!monitor.refresh(roots.clone(), now).await);
    finish_sample(&monitor).await;
    assert!(monitor.refresh(roots.clone(), now).await);

    // Assert
    assert_eq!(monitor.values["session"].process_count, 1);
    assert!(!monitor.refresh(roots, now).await);
    assert!(
        monitor
            .refresh(HashMap::from([(SessionId::from("session"), 20)]), now)
            .await
    );
    assert!(monitor.values.is_empty());
    assert!(!monitor.refresh(HashMap::new(), now).await);
}

#[tokio::test]
async fn missing_root_clears_previous_sample_even_when_descendants_remain() {
    // Arrange
    let mut client = MockResourceClient::new();
    let mut sequence = mockall::Sequence::new();
    let resources = SessionResources {
        cpu_percent: 12.5,
        process_count: 1,
        resident_memory_kib: 2048,
    };
    client
        .expect_sample()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(move |_| {
            Some(vec![ProcessSample {
                host_cpu_temperature_celsius: None,
                is_alive: true,
                identity: Some(ProcessIdentity(1_000_001)),
                parent_pid: 1,
                pid: 10,
                resources,
            }])
        });
    client
        .expect_sample()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(move |_| {
            Some(vec![ProcessSample {
                host_cpu_temperature_celsius: None,
                is_alive: true,
                identity: Some(ProcessIdentity(1_000_001)),
                parent_pid: 10,
                pid: 20,
                resources,
            }])
        });
    let mut monitor = ResourceMonitor::new(Arc::new(client));
    let roots = HashMap::from([(SessionId::from("session"), 10)]);
    let now = Instant::now();

    // Act
    monitor.refresh(roots.clone(), now).await;
    finish_sample(&monitor).await;
    monitor.refresh(roots.clone(), now).await;
    let previous_resources = monitor.values["session"];
    let next_sample = now + Duration::from_secs(2);
    monitor.refresh(roots.clone(), next_sample).await;
    finish_sample(&monitor).await;
    let changed = monitor.refresh(roots, next_sample).await;

    // Assert
    assert_eq!(previous_resources, resources);
    assert!(changed);
    assert!(monitor.values.is_empty());
}

#[tokio::test]
async fn stale_results_and_failed_samples_are_discarded() {
    // Arrange
    let mut client = MockResourceClient::new();
    client
        .expect_sample()
        .times(1)
        .returning(|_| Some(Vec::new()));
    client.expect_sample().times(1).returning(|_| None);
    let mut monitor = ResourceMonitor::new(Arc::new(client));
    let roots = HashMap::from([(SessionId::from("session"), 10)]);
    let now = Instant::now();

    // Act
    monitor.refresh(roots.clone(), now).await;
    finish_sample(&monitor).await;
    monitor.refresh(HashMap::new(), now).await;
    monitor
        .refresh(roots.clone(), now + Duration::from_secs(2))
        .await;
    finish_sample(&monitor).await;
    monitor.refresh(roots, now + Duration::from_secs(2)).await;

    // Assert
    assert!(monitor.values.is_empty());
}
