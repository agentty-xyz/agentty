use super::FrameTime;

#[test]
fn frame_time_exposes_one_coherent_snapshot() {
    // Arrange
    let frame_time = FrameTime::new(1_700_000_000, 1_700_000_000_125, -28_800);

    // Act & Assert
    assert_eq!(frame_time.unix_seconds(), 1_700_000_000);
    assert_eq!(frame_time.unix_millis(), 1_700_000_000_125);
    assert_eq!(frame_time.local_utc_offset_seconds(), -28_800);
}

#[test]
fn local_utc_offset_seconds_at_uses_resolved_offset_only_for_its_timestamp() {
    // Arrange
    let frame_time =
        FrameTime::new(1_700_000_000, 0, -28_800).with_local_utc_offset_at(1_690_000_000, -25_200);

    // Act
    let resolved = frame_time.local_utc_offset_seconds_at(1_690_000_000);
    let unresolved = frame_time.local_utc_offset_seconds_at(1_690_000_001);
    let without_resolution =
        FrameTime::new(1_700_000_000, 0, -28_800).local_utc_offset_seconds_at(1_690_000_000);

    // Assert
    assert_eq!(resolved, -25_200);
    assert_eq!(unresolved, -28_800);
    assert_eq!(without_resolution, -28_800);
    assert_eq!(frame_time.local_utc_offset_seconds(), -28_800);
}
