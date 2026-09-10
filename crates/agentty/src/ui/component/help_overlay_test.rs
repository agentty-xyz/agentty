use ratatui::layout::Rect;

use super::{HelpOverlay, OVERLAY_DIMENSIONS};
use crate::presentation::app_mode::HelpContext;
use crate::presentation::help_action::HelpAction;

#[test]
fn test_popup_area_centers_within_area() {
    // Arrange

    let area = Rect::new(0, 0, 100, 50);

    // Act

    let popup = OVERLAY_DIMENSIONS.centered_popup_area(area);

    // Assert

    assert_eq!(popup.width, 40);

    assert_eq!(popup.height, 30);

    assert_eq!(popup.x, 30);

    assert_eq!(popup.y, 10);
}

#[test]
fn test_popup_area_clamps_to_area_when_small() {
    // Arrange

    let area = Rect::new(0, 0, 20, 8);

    // Act

    let popup = OVERLAY_DIMENSIONS.centered_popup_area(area);

    // Assert — min sizes clamped to area

    assert_eq!(popup.width, 20);

    assert_eq!(popup.height, 8);
}

#[test]
fn test_popup_area_respects_minimum_dimensions() {
    // Arrange

    let area = Rect::new(0, 0, 40, 20);

    // Act

    let popup = OVERLAY_DIMENSIONS.centered_popup_area(area);

    // Assert — 40% of 40=16 < MIN 30, so width = 30; 60% of 20=12 >= MIN 10

    assert_eq!(popup.width, 30);

    assert_eq!(popup.height, 12);
}

#[test]
fn test_help_overlay_new_stores_fields() {
    // Arrange
    let context = HelpContext::List {
        keybindings: vec![HelpAction::new("quit", "q", "Quit")],
    };

    // Act
    let overlay = HelpOverlay::new(&context).scroll_offset(5);

    // Assert
    assert_eq!(overlay.scroll_offset, 5);
}
