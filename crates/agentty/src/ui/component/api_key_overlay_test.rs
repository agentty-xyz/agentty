//! API key popup rendering unit tests.

use super::{ApiKeyOverlay, HELP_TEXT, INPUT_PLACEHOLDER};
use crate::presentation::api_key::API_KEY_TITLE;
use crate::ui::render::Component;

/// Renders `overlay` on a 120×40 terminal and returns its text.
fn rendered_text(overlay: &ApiKeyOverlay<'_>) -> String {
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            overlay.render(frame, area);
        })
        .expect("failed to draw");

    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

#[test]
fn an_empty_field_shows_the_message_placeholder_and_help() {
    // Arrange
    let overlay = ApiKeyOverlay::new("Needs a key.\n\nSaved privately.", "", 0);

    // Act
    let text = rendered_text(&overlay);

    // Assert
    assert!(text.contains(API_KEY_TITLE));
    assert!(text.contains("Needs a key."));
    assert!(text.contains("Saved privately."));
    assert!(text.contains(INPUT_PLACEHOLDER));
    assert!(text.contains(HELP_TEXT));
}

#[test]
fn a_pasted_key_renders_only_its_mask() {
    // Arrange
    let overlay = ApiKeyOverlay::new("Needs a key.", "•••••", 5);

    // Act
    let text = rendered_text(&overlay);

    // Assert
    assert!(text.contains("•••••"));
    assert!(!text.contains(INPUT_PLACEHOLDER));
}
