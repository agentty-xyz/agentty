use ratatui::style::{Modifier, Style};

use super::{footer_line, footer_muted_span, footer_separator_span};
use crate::presentation::help_action::HelpAction;
use crate::ui::style;

#[test]
fn footer_line_styles_keys_labels_and_separator() {
    // Arrange
    let actions = vec![
        HelpAction::new("quit", "q", "Quit"),
        HelpAction::new("help", "?", "Help"),
    ];

    // Act
    let line = footer_line(&actions);

    // Assert
    assert_eq!(line.to_string(), "q: quit | ?: help");
    assert_eq!(
        line.spans[0].style,
        Style::default()
            .fg(style::palette::accent())
            .add_modifier(Modifier::BOLD)
    );
    assert_eq!(
        line.spans[3].style,
        Style::default().fg(style::palette::text_subtle())
    );
}

#[test]
fn footer_muted_span_uses_muted_style() {
    // Arrange & Act
    let span = footer_muted_span("note");

    // Assert
    assert_eq!(span.content, "note");
    assert_eq!(
        span.style,
        Style::default().fg(style::palette::text_muted())
    );
}

#[test]
fn footer_separator_span_uses_subtle_style() {
    // Arrange & Act
    let span = footer_separator_span();

    // Assert
    assert_eq!(span.content, " | ");
    assert_eq!(
        span.style,
        Style::default().fg(style::palette::text_subtle())
    );
}
