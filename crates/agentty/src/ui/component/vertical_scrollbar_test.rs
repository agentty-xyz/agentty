use super::VerticalScrollbar;

#[test]
fn test_thumb_geometry_clamps_overscroll_to_track_bottom() {
    // Arrange
    let scrollbar = VerticalScrollbar::new(u16::MAX, 40);

    // Act
    let (thumb_offset, thumb_height) = scrollbar.thumb_geometry(8);

    // Assert
    assert_eq!(thumb_offset + thumb_height, 8);
}
