use super::VerticalScrollbar;

#[test]
fn test_thumb_geometry_clamps_overscroll_to_track_bottom() {
    // Arrange
    let scrollbar = VerticalScrollbar::new(usize::MAX, 40);

    // Act
    let (thumb_offset, thumb_height) = scrollbar.thumb_geometry(8);

    // Assert
    assert_eq!(thumb_offset + thumb_height, 8);
}

#[test]
fn test_thumb_geometry_for_large_transcript() {
    // Arrange
    let scrollbar = VerticalScrollbar::new(69_990, 70_000);

    // Act
    let geometry = scrollbar.thumb_geometry(10);

    // Assert
    assert_eq!(geometry, (9, 1));
}
