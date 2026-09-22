use super::{MOUSE_WHEEL_SCROLL_LINES, ScrollRegion, ScrollbarGeometry};
use crate::presentation::app_mode::ViewportRect;

fn region(total_lines: u16, viewport_height: u16) -> ScrollRegion {
    ScrollRegion {
        area: ViewportRect {
            height: viewport_height + 2,
            width: 40,
            x: 5,
            y: 3,
        },
        scrollbar: Some(ViewportRect {
            height: viewport_height,
            width: 1,
            x: 44,
            y: 4,
        }),
        total_lines,
        viewport_height,
    }
}

#[test]
fn test_contains_uses_panel_area_bounds() {
    // Arrange
    let region = region(50, 10);

    // Act & Assert
    assert!(region.contains(5, 3));
    assert!(region.contains(44, 14));
    assert!(!region.contains(45, 3));
    assert!(!region.contains(5, 15));
}

#[test]
fn test_scroll_down_and_up_clamp_to_content_range() {
    // Arrange
    let region = region(50, 10);

    // Act
    let scrolled_down = region.scroll_down(38, MOUSE_WHEEL_SCROLL_LINES);
    let scrolled_up = region.scroll_up(2, MOUSE_WHEEL_SCROLL_LINES);

    // Assert
    assert_eq!(scrolled_down, 40);
    assert_eq!(scrolled_up, 0);
}

#[test]
fn test_scroll_tail_down_returns_none_at_bottom() {
    // Arrange
    let region = region(50, 10);

    // Act
    let still_pinned = region.scroll_tail_down(Some(10), 3);
    let follows_tail = region.scroll_tail_down(Some(38), 3);
    let stays_following = region.scroll_tail_down(None, 3);

    // Assert
    assert_eq!(still_pinned, Some(13));
    assert_eq!(follows_tail, None);
    assert_eq!(stays_following, None);
}

#[test]
fn test_scroll_tail_up_pins_from_follow_tail() {
    // Arrange
    let region = region(50, 10);

    // Act
    let pinned = region.scroll_tail_up(None, 3);

    // Assert
    assert_eq!(pinned, 37);
}

#[test]
fn test_scrollbar_geometry_round_trips_scroll_offset() {
    // Arrange
    let track_height = 10;
    let total_lines = 100;

    // Act & Assert
    for scroll_offset in [0, 1, 45, 89, 90] {
        let geometry = ScrollbarGeometry::new(track_height, total_lines, scroll_offset);
        let recovered = geometry.scroll_offset_for_thumb_row(geometry.thumb_offset);
        let recovered_geometry = ScrollbarGeometry::new(track_height, total_lines, recovered);

        assert_eq!(recovered_geometry.thumb_offset, geometry.thumb_offset);
    }
}

#[test]
fn test_scrollbar_geometry_maps_track_ends_to_scroll_extremes() {
    // Arrange
    let geometry = ScrollbarGeometry::new(10, 100, 0);

    // Act
    let top = geometry.scroll_offset_for_thumb_row(0);
    let bottom = geometry.scroll_offset_for_thumb_row(9);
    let beyond = geometry.scroll_offset_for_thumb_row(50);

    // Assert
    assert_eq!(top, 0);
    assert_eq!(bottom, 90);
    assert_eq!(beyond, 90);
}

#[test]
fn test_scrollbar_geometry_without_overflow_has_no_scroll_range() {
    // Arrange
    let geometry = ScrollbarGeometry::new(10, 5, 0);

    // Act & Assert
    assert_eq!(geometry.thumb_height, 10);
    assert_eq!(geometry.scroll_offset_for_thumb_row(4), 0);
}

#[test]
fn test_thumb_grab_row_detects_thumb_and_track_presses() {
    // Arrange
    let region = region(100, 10);

    // Act
    let grabbed = region.thumb_grab_row(0, 4);
    let missed = region.thumb_grab_row(0, 9);

    // Assert
    assert_eq!(grabbed, Some(0));
    assert_eq!(missed, None);
}

#[test]
fn test_scroll_offset_for_pointer_row_anchors_grab_offset() {
    // Arrange
    let region = region(100, 10);

    // Act
    let dragged_to_bottom = region.scroll_offset_for_pointer_row(13, 0);
    let dragged_with_grab = region.scroll_offset_for_pointer_row(9, 1);

    // Assert
    assert_eq!(dragged_to_bottom, 90);
    assert_eq!(dragged_with_grab, 40);
}

#[test]
fn test_scroll_offset_for_pointer_row_without_scrollbar_stays_at_top() {
    // Arrange
    let region = ScrollRegion {
        scrollbar: None,
        ..region(100, 10)
    };

    // Act
    let scroll_offset = region.scroll_offset_for_pointer_row(7, 2);

    // Assert
    assert_eq!(scroll_offset, 0);
}
