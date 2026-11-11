use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::VerticalScrollbar;
use crate::ui::Component;

#[test]
fn test_render_skips_content_that_fits_the_track() {
    // Arrange
    let mut terminal = Terminal::new(TestBackend::new(1, 4)).expect("failed to create terminal");
    let scrollbar = VerticalScrollbar::new(0, 4);

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            scrollbar.render(frame, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    assert!(buffer.content().iter().all(|cell| cell.symbol() == " "));
}

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
