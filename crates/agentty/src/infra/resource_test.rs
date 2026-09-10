use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::{
    RealResourceClient, ResourceClient, TemperatureCache, hottest_cpu_temperature,
    parse_process_table, process_tree_resources,
};
use crate::domain::resource::SessionResources;
use crate::infra::process_identity::ProcessIdentity;

/// Bounds test waits for controlled workers that are expected to finish.
fn finish_temperature_read(cache: &TemperatureCache) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !cache.pending.as_ref().expect("sensor worker").is_finished() {
        assert!(Instant::now() < deadline, "sensor worker stalled");
        thread::yield_now();
    }
}

/// Cached lookups and completed reads must not launch replacement workers.
fn assert_cached_temperature(cache: &mut TemperatureCache, now: Instant, expected: Option<f32>) {
    assert_eq!(cache.sample(now, || None), expected);
    assert!(cache.pending.is_none(), "cached lookup started a worker");
}

#[test]
fn temperature_cache_throttles_discovery_and_replaces_failed_readings() {
    // Arrange
    let mut cache = TemperatureCache::default();
    let now = Instant::now();

    // Act / Assert
    assert_eq!(cache.sample(now, || Some(64.5)), None);
    finish_temperature_read(&cache);
    assert_cached_temperature(&mut cache, now, Some(64.5));
    assert_cached_temperature(&mut cache, now + Duration::from_secs(9), Some(64.5));
    assert_eq!(
        cache.sample(now + Duration::from_secs(10), || None),
        Some(64.5)
    );
    finish_temperature_read(&cache);
    assert_cached_temperature(&mut cache, now + Duration::from_secs(10), None);
    assert_cached_temperature(&mut cache, now + Duration::from_secs(19), None);
    assert_eq!(
        cache.sample(now + Duration::from_secs(20), || Some(70.0)),
        None
    );
    finish_temperature_read(&cache);
    assert_cached_temperature(&mut cache, now + Duration::from_secs(20), Some(70.0));
}

#[test]
fn stalled_temperature_read_expires_old_value_without_spawning_more_workers() {
    // Arrange
    let now = Instant::now();
    let mut cache = TemperatureCache {
        sample: Some((now, Some(64.5))),
        ..TemperatureCache::default()
    };
    let (release, wait) = mpsc::channel();

    // Act / Assert
    assert_eq!(
        cache.sample(now + Duration::from_secs(10), move || wait.recv().ok()),
        Some(64.5)
    );
    let worker_id = cache.pending.as_ref().expect("sensor worker").thread().id();
    for (seconds, expected) in [(12, Some(64.5)), (20, None), (100, None)] {
        assert_eq!(
            cache.sample(now + Duration::from_secs(seconds), || None),
            expected
        );
        assert_eq!(
            cache.pending.as_ref().expect("sensor worker").thread().id(),
            worker_id
        );
    }
    release.send(70.0).expect("release sensor worker");
    finish_temperature_read(&cache);
    // The late result has expired; a new read starts only after it exits.
    assert_eq!(
        cache.sample(now + Duration::from_secs(100), || Some(70.0)),
        None
    );
    finish_temperature_read(&cache);
    assert_cached_temperature(&mut cache, now + Duration::from_secs(100), Some(70.0));
}

#[test]
fn panicked_temperature_worker_is_unavailable_and_retries_after_cooldown() {
    // Arrange: a worker incorrectly assumes its input channel is connected.
    let mut cache = TemperatureCache::default();
    let now = Instant::now();
    let (sender, receiver) = mpsc::channel::<Option<f32>>();
    drop(sender);

    // Act / Assert
    assert_eq!(
        cache.sample(now, move || receiver
            .recv()
            .expect("sensor input disconnected")),
        None
    );
    finish_temperature_read(&cache);
    assert_cached_temperature(&mut cache, now, None);
    assert_eq!(
        cache.sample(now + Duration::from_secs(10), || Some(70.0)),
        None
    );
    finish_temperature_read(&cache);
    assert_cached_temperature(&mut cache, now + Duration::from_secs(10), Some(70.0));
}

#[test]
fn failed_temperature_worker_start_is_throttled_before_retrying() {
    // Arrange: a worker could not be started on the previous attempt.
    let now = Instant::now();
    let mut cache = TemperatureCache {
        last_started: Some(now),
        ..TemperatureCache::default()
    };

    // Act / Assert
    assert_cached_temperature(&mut cache, now + Duration::from_secs(9), None);
    assert_eq!(
        cache.sample(now + Duration::from_secs(10), || Some(70.0)),
        None
    );
    finish_temperature_read(&cache);
    assert_cached_temperature(&mut cache, now + Duration::from_secs(10), Some(70.0));
}

#[tokio::test]
async fn stalled_temperature_worker_does_not_block_process_accounting() {
    // Arrange
    let client = RealResourceClient::default();
    let (release, wait) = mpsc::channel();
    client
        .temperature
        .lock()
        .expect("cache")
        .sample(Instant::now(), move || wait.recv().ok());

    // Act / Assert
    for _ in 0..3 {
        let samples = tokio::time::timeout(
            Duration::from_secs(3),
            client.sample(vec![std::process::id()]),
        )
        .await
        .expect("accounting must not wait for sensors")
        .expect("host ps available");
        let resources = process_tree_resources(&samples, std::process::id()).expect("root");
        assert!(resources.process_count >= 1);
        assert!(resources.resident_memory_kib > 0);
        assert!(
            !client
                .temperature
                .lock()
                .expect("cache")
                .pending
                .as_ref()
                .expect("worker")
                .is_finished()
        );
    }
    release.send(64.5).expect("release sensor worker");
    finish_temperature_read(&client.temperature.lock().expect("cache"));
}

#[cfg(debug_assertions)]
#[test]
fn temperature_overrides_pin_recordings_and_invalid_values_use_live_sensors() {
    // Arrange
    let read = || Some(42.0);

    // Act / Assert
    assert_eq!(
        RealResourceClient::temperature_with_override(Some("64.5"), read),
        Some(64.5)
    );
    assert_eq!(
        RealResourceClient::temperature_with_override(Some("--"), read),
        None
    );
    for pinned in [
        None,
        Some(""),
        Some("invalid"),
        Some("NaN"),
        Some("inf"),
        Some("-1"),
        Some("0"),
    ] {
        assert_eq!(
            RealResourceClient::temperature_with_override(pinned, read),
            Some(42.0)
        );
    }
    assert_eq!(
        RealResourceClient::temperature_with_override(None, || None),
        None
    );
}

#[test]
fn cpu_temperature_uses_hottest_valid_cpu_sensor() {
    // Arrange
    let sensors = [
        ("coretemp Package id 0", Some(61.0)),
        ("k10temp Tdie", Some(62.0)),
        ("zenpower Tdie", Some(63.0)),
        ("CPU Proximity", Some(64.5)),
        ("pACC MTR Temp Sensor0", Some(62.5)),
        ("eACC MTR Temp Sensor0", Some(61.5)),
        ("GPU", Some(95.0)),
        ("nvme Composite", Some(90.0)),
        ("CPU Core 1", None),
        ("CPU Core 2", Some(f32::NAN)),
        ("CPU Core 3", Some(f32::INFINITY)),
        ("CPU Core 4", Some(-1.0)),
        ("CPU Core 5", Some(0.0)),
    ];

    // Act / Assert
    for (label, temperature) in &sensors[..6] {
        assert_eq!(
            hottest_cpu_temperature([(*label, *temperature)]),
            *temperature,
            "{label}",
        );
    }
    assert_eq!(hottest_cpu_temperature(sensors), Some(64.5));
    assert_eq!(hottest_cpu_temperature([]), None);
    assert_eq!(hottest_cpu_temperature([("GPU", Some(95.0))]), None);
    assert_eq!(hottest_cpu_temperature([("CPU", None)]), None);
}

#[test]
fn m5_cpu_temperature_recognizes_numbered_die_probes_only() {
    // Arrange
    let cpu_sensors = [("PMU tdie1", Some(66.5)), ("PMU tdie14", Some(59.2))];
    let unrelated_sensors = [
        ("PMU2 tdie1", Some(90.0)),
        ("PMU tdev1", Some(90.0)),
        ("PMU tcal", Some(90.0)),
        ("gas gauge battery", Some(90.0)),
        ("NAND CH0 temp", Some(90.0)),
        ("GPU MTR Temp Sensor0", Some(90.0)),
        ("PMU tdie", Some(90.0)),
        ("PMU tdie1 extra", Some(90.0)),
    ];

    // Act
    let temperature = hottest_cpu_temperature(cpu_sensors.into_iter().chain(unrelated_sensors));

    // Assert
    assert_eq!(temperature, Some(66.5));
    for sensor in cpu_sensors {
        assert_eq!(hottest_cpu_temperature([sensor]), sensor.1);
    }
    for sensor in unrelated_sensors {
        assert_eq!(hottest_cpu_temperature([sensor]), None, "{}", sensor.0);
    }
    for invalid in [
        None,
        Some(0.0),
        Some(-1.0),
        Some(f32::NAN),
        Some(f32::INFINITY),
    ] {
        assert_eq!(hottest_cpu_temperature([("PMU tdie1", invalid)]), None);
    }
}

#[test]
fn host_temperature_is_preserved_without_summing_descendants() {
    // Arrange
    let mut samples = parse_process_table("1 0 1 100 S\n2 1 2 200 S").expect("table");
    for sample in &mut samples {
        sample.host_cpu_temperature_celsius = Some(64.5);
    }

    // Act
    let resources = process_tree_resources(&samples, 1).expect("root");

    // Assert
    assert_eq!(samples[0].host_cpu_temperature_celsius, Some(64.5));
    assert_eq!(resources.process_count, 2);
}

#[cfg(unix)]
#[test]
fn failed_command_and_invalid_utf8_are_unavailable() {
    // Arrange
    let mut output = std::process::Output {
        status: std::process::ExitStatus::from_raw(256),
        stdout: b"1 0 0 1024".to_vec(),
        stderr: Vec::new(),
    };

    // Act / Assert
    assert!(RealResourceClient::parse_output(&output).is_none());
    output.status = std::process::ExitStatus::from_raw(0);
    output.stdout = vec![0xff];
    assert!(RealResourceClient::parse_output(&output).is_none());
}

#[test]
fn tree_totals_include_descendants_and_exclude_other_sessions() {
    // Arrange
    let samples = parse_process_table(
        "30 20 25.5 1024 S\n10 1 100.0 2048 Ss\n20 10 3.0 512 R\n40 1 90.0 8000 S\n",
    )
    .expect("valid table");

    // Act
    let resources = process_tree_resources(&samples, 10).expect("root present");

    // Assert
    assert_eq!(
        resources,
        SessionResources {
            cpu_percent: 128.5,
            process_count: 3,
            resident_memory_kib: 3584
        }
    );
    assert_eq!(process_tree_resources(&samples, 999), None);
    assert_eq!(process_tree_resources(&samples, 1), None);
}

#[test]
fn malformed_tables_are_unavailable() {
    // Arrange
    let malformed = [
        "1",
        "x 0 0 1 S",
        "1 x 0 1 S",
        "1 0 x 1 S",
        "1 0 0 x S",
        "1 0 NaN 1 S",
        "1 0 inf 1 S",
        "1 0 -1 1 S",
        "1 0 0 1",
        "1 0 0 1 S extra",
    ];

    // Act / Assert
    for output in malformed {
        assert!(parse_process_table(output).is_none(), "{output}");
    }
    assert!(parse_process_table("\n ").expect("empty table").is_empty());
}

#[test]
fn cycles_and_duplicate_rows_do_not_double_count() {
    // Arrange
    let samples =
        parse_process_table("1 2 1 100 S\n2 1 2 200 S\n2 1 2 200 S").expect("valid table");

    // Act
    let resources = process_tree_resources(&samples, 1).expect("root present");

    // Assert
    assert_eq!(resources.process_count, 2);
    assert_eq!(resources.resident_memory_kib, 300);
}

#[test]
fn exited_roots_are_unavailable_and_exited_children_are_excluded() {
    // Arrange
    let samples = parse_process_table("1 0 1 100 S\n2 1 50 200 Z+\n3 1 50 200 X\n4 1 50 200 x")
        .expect("valid table");

    // Act
    let resources = process_tree_resources(&samples, 1).expect("live root");

    // Assert
    assert_eq!(resources.process_count, 1);
    assert_eq!(resources.cpu_percent, 1.0);
    assert_eq!(resources.resident_memory_kib, 100);
    for pid in [2, 3, 4] {
        assert!(process_tree_resources(&samples, pid).is_none());
    }
}

#[test]
fn native_identity_changes_within_one_second_cannot_bind_stale_accounting() {
    // Arrange
    let original = ProcessIdentity(1_000_001);
    let reused = ProcessIdentity(1_000_002);
    let before = HashMap::from([(10, original), (20, original), (30, original)]);
    let after = HashMap::from([(10, original), (20, reused), (40, original)]);
    let mut samples =
        parse_process_table("10 1 1 100 S\n20 1 90 8192 S\n30 1 1 100 S\n40 1 1 100 S")
            .expect("accounting snapshot");

    // Act
    RealResourceClient::bind_identities(&mut samples, &before, &after);

    // Assert
    assert_eq!(original.0 / 1_000_000, reused.0 / 1_000_000);
    assert_eq!(samples[0].identity, Some(original));
    assert!(samples[1..].iter().all(|sample| sample.identity.is_none()));

    // Act: a later coherent sample identifies the new process distinctly.
    RealResourceClient::bind_identities(&mut samples, &after, &after);

    // Assert
    assert_eq!(samples[1].identity, Some(reused));
    assert_ne!(samples[1].identity, Some(original));
}

#[tokio::test]
async fn real_process_table_contains_this_process() {
    // Arrange
    let client = RealResourceClient::default();

    // Act
    let samples = tokio::time::timeout(
        Duration::from_secs(3),
        client.sample(vec![std::process::id(), u32::MAX]),
    )
    .await
    .expect("process accounting must complete without waiting for sensors")
    .expect("host ps available");
    let resources = process_tree_resources(&samples, std::process::id()).expect("root present");

    // Assert
    assert_eq!(
        samples
            .iter()
            .find(|sample| sample.pid == std::process::id())
            .expect("root")
            .identity,
        ProcessIdentity::read(std::process::id()),
    );
    assert!(resources.process_count >= 1);
    assert!(resources.resident_memory_kib > 0);
}
