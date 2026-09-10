use super::ConfirmationOverlay;
use crate::ui::Component;

#[test]
fn test_confirmation_overlay_new_stores_fields() {
    // Arrange
    let message = "Delete session?";
    let selected_first = false;
    let title = "Confirm";

    // Act
    let overlay = ConfirmationOverlay::new(title, message).selected_first(selected_first);

    // Assert
    assert_eq!(overlay.message, message);
    assert_eq!(overlay.selected_first, selected_first);
    assert_eq!(overlay.title, title);
}

#[test]
fn test_confirmation_overlay_render_hides_bottom_navigation_hints() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(100, 20);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let overlay =
        ConfirmationOverlay::new("Confirm Delete", "Delete session?").selected_first(false);

    // Act
    terminal
        .draw(|f| {
            let area = f.area();
            Component::render(&overlay, f, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let content = buffer.content();
    let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
    assert!(text.contains("Yes"));
    assert!(text.contains("No"));
    assert!(!text.contains("Left/Right"));
    assert!(!text.contains(": choose"));
    assert!(!text.contains(": select"));
}

#[test]
fn test_confirmation_overlay_render_preserves_choices_for_long_message() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(100, 20);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let message = "Delete session \"session with a very long name that keeps going and would \
                   otherwise hide choices in the confirmation popup\"?";
    let overlay = ConfirmationOverlay::new("Confirm Delete", message).selected_first(false);

    // Act
    terminal
        .draw(|f| {
            let area = f.area();
            Component::render(&overlay, f, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let content = buffer.content();
    let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
    assert!(text.contains("Yes"));
    assert!(text.contains("No"));
    assert!(text.contains("..."));
}

#[test]
fn confirmation_overlay_renders_custom_binary_choice_labels() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(80, 20);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let overlay = ConfirmationOverlay::new("Integration Approach", "Choose a destination")
        .option_labels("Local merges", "Review requests")
        .selected_first(true);

    // Act
    terminal
        .draw(|frame| overlay.render(frame, frame.area()))
        .expect("failed to draw");

    // Assert
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect::<String>();
    assert!(text.contains("Local merges"));
    assert!(text.contains("Review requests"));
}
