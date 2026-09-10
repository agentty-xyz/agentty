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
