use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{
    Clock, FixedClock, RealClock, from_environment_values, parse_utc_offset_seconds,
    unix_timestamp_millis, unix_timestamp_seconds,
};

/// Minimal clock fixture that exercises the trait's default UTC offset.
struct DefaultOffsetClock;

impl Clock for DefaultOffsetClock {
    fn now_instant(&self) -> Instant {
        Instant::now()
    }

    fn now_system_time(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }
}

#[test]
fn fixed_clock_freezes_wall_clock_time() {
    // Arrange
    let system_time = SystemTime::UNIX_EPOCH + Duration::from_secs(90);
    let clock = FixedClock {
        local_utc_offset_seconds: 0,
        system_time,
    };

    // Act
    let first_read = clock.now_system_time();
    let second_read = clock.now_system_time();

    // Assert
    assert_eq!(first_read, system_time);
    assert_eq!(second_read, system_time);
}

#[test]
fn fixed_clock_keeps_monotonic_time_advancing() {
    // Arrange
    let clock = FixedClock {
        local_utc_offset_seconds: 0,
        system_time: SystemTime::UNIX_EPOCH,
    };

    // Act
    let earlier = clock.now_instant();
    let later = clock.now_instant();

    // Assert
    assert!(later >= earlier);
}

#[test]
fn unix_timestamp_millis_preserves_subsecond_time() {
    // Arrange
    let system_time = UNIX_EPOCH + Duration::from_millis(1_234);

    // Act
    let timestamp_millis = unix_timestamp_millis(system_time);

    // Assert
    assert_eq!(timestamp_millis, 1_234);
}

#[test]
fn unix_timestamp_seconds_uses_injected_wall_clock() {
    // Arrange
    let clock = FixedClock {
        local_utc_offset_seconds: 0,
        system_time: UNIX_EPOCH + Duration::from_millis(1_234),
    };

    // Act
    let timestamp_seconds = unix_timestamp_seconds(&clock);

    // Assert
    assert_eq!(timestamp_seconds, 1);
}

#[test]
fn fixed_clock_freezes_utc_offset() {
    // Arrange
    let clock = FixedClock {
        local_utc_offset_seconds: -28_800,
        system_time: SystemTime::UNIX_EPOCH,
    };

    // Act
    let utc_offset_seconds = clock.local_utc_offset_seconds(123);

    // Assert
    assert_eq!(utc_offset_seconds, -28_800);
}

#[test]
fn clock_trait_defaults_utc_offset_to_zero() {
    // Arrange
    let clock = DefaultOffsetClock;

    // Act
    let utc_offset_seconds = clock.local_utc_offset_seconds(123);
    let monotonic_time = clock.now_instant();
    let system_time = clock.now_system_time();

    // Assert
    assert_eq!(utc_offset_seconds, 0);
    assert!(monotonic_time <= Instant::now());
    assert_eq!(system_time, SystemTime::UNIX_EPOCH);
}

#[test]
fn environment_values_build_fixed_clock_with_pinned_offset() {
    // Arrange
    let unix_seconds = Some("90");
    let utc_offset_seconds = Some("-28800");

    // Act
    let clock = from_environment_values(unix_seconds, utc_offset_seconds, None);

    // Assert
    assert_eq!(
        clock.now_system_time(),
        SystemTime::UNIX_EPOCH + Duration::from_secs(90)
    );
    assert_eq!(clock.local_utc_offset_seconds(123), -28_800);
}

#[test]
fn environment_values_ignore_controls_without_valid_fixed_time() {
    // Arrange
    let directory = tempfile::tempdir().expect("clock fixture directory");
    let path = directory.path().join("advance");
    std::fs::write(&path, "10000").expect("clock fixture advance");

    for unix_seconds in [None, Some("invalid")] {
        // Act
        let before = Instant::now();
        let clock = from_environment_values(unix_seconds, Some("-28800"), Some(path.clone()));
        let instant = clock.now_instant();
        let after = Instant::now();

        // Assert
        assert!(clock.now_system_time() > SystemTime::UNIX_EPOCH);
        assert!((before..=after).contains(&instant));
    }
}

#[test]
fn environment_values_default_invalid_offset_to_utc() {
    // Arrange
    let unix_seconds = Some("90");
    let utc_offset_seconds = Some("invalid");

    // Act
    let clock = from_environment_values(unix_seconds, utc_offset_seconds, None);

    // Assert
    assert_eq!(clock.local_utc_offset_seconds(123), 0);
}

#[test]
fn pinned_clock_accepts_only_forward_bounded_file_advances() {
    // Arrange
    let directory = tempfile::tempdir().expect("clock fixture directory");
    let path = directory.path().join("advance");
    let clock = from_environment_values(Some("90"), Some("-28800"), Some(path.clone()));
    let mut previous = clock.now_instant();

    for (contents, expected_millis) in [
        (None, 0),
        (Some("invalid"), 0),
        (Some("-1"), 0),
        (Some("4294967296"), 0),
        (Some(" 10500\n"), 10_500),
        (Some("4"), 10_500),
        (Some(""), 10_500),
        (Some("20000"), 20_000),
        (None, 20_000),
        (Some("4294967295"), u64::from(u32::MAX)),
    ] {
        if let Some(contents) = contents {
            std::fs::write(&path, contents).expect("write clock advance");
        } else if path.exists() {
            std::fs::remove_file(&path).expect("remove clock advance");
        }

        // Act
        let offset = Duration::from_millis(expected_millis);
        let before = Instant::now() + offset;
        let instant = clock.now_instant();
        let after = Instant::now() + offset;

        // Assert
        assert!((before..=after).contains(&instant), "advance: {contents:?}");
        assert!(instant >= previous);
        assert_eq!(
            clock.now_system_time(),
            UNIX_EPOCH + Duration::from_secs(90)
        );
        assert_eq!(clock.local_utc_offset_seconds(123), -28_800);
        previous = instant;
    }
}

#[test]
fn utc_offset_parser_rejects_invalid_and_out_of_range_values() {
    // Arrange, Act, Assert
    assert_eq!(parse_utc_offset_seconds(" 3600 "), Some(3_600));
    assert_eq!(parse_utc_offset_seconds("invalid"), None);
    assert_eq!(parse_utc_offset_seconds("1000000"), None);
}

#[test]
fn real_clock_offset_falls_back_for_invalid_timestamp() {
    // Arrange
    let invalid_timestamp = i64::MAX;
    let clock = RealClock;

    // Act
    let utc_offset_seconds = clock.local_utc_offset_seconds(invalid_timestamp);

    // Assert
    assert_eq!(utc_offset_seconds, 0);
}

#[test]
fn real_clock_advances_wall_clock_time() {
    // Arrange
    let clock = RealClock;

    // Act
    let now = clock.now_system_time();

    // Assert
    assert!(now > SystemTime::UNIX_EPOCH);
}
