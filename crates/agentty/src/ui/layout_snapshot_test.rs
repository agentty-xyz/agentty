use ratatui::layout::Rect;

use super::{
    begin_frame, record_chat_output, record_diff_file_list, record_help_overlay, scroll_region,
    take_frame,
};
use crate::presentation::app_mode::ViewportRect;
use crate::presentation::viewport::LayoutSnapshot;

#[test]
fn test_take_frame_returns_recorded_regions_and_resets() {
    // Arrange
    begin_frame();
    let region = scroll_region(Rect::new(1, 2, 30, 12), None, 40, 10);

    // Act
    record_chat_output(region);
    record_diff_file_list(Rect::new(0, 0, 10, 5));
    let snapshot = take_frame();
    let emptied = take_frame();

    // Assert
    assert_eq!(snapshot.chat_output, Some(region));
    assert_eq!(
        snapshot.diff_file_list,
        Some(ViewportRect {
            height: 5,
            width: 10,
            x: 0,
            y: 0
        })
    );
    assert_eq!(emptied, LayoutSnapshot::default());
}

#[test]
fn test_begin_frame_clears_previous_regions() {
    // Arrange
    record_help_overlay(scroll_region(Rect::new(0, 0, 5, 5), None, 3, 3));

    // Act
    begin_frame();
    let snapshot = take_frame();

    // Assert
    assert_eq!(snapshot.help_overlay, None);
}
