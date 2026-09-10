//! Ratatui formatting for frontend-neutral help-action contracts.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::presentation::help_action::HelpAction;
use crate::ui::style;

/// Renders one-line footer help with emphasized keys and muted labels.
pub(crate) fn footer_line(actions: &[HelpAction]) -> Line<'static> {
    let mut spans = Vec::new();

    for (index, action) in actions.iter().enumerate() {
        if index > 0 {
            spans.push(footer_separator_span());
        }

        spans.push(footer_key_span(action.key));
        spans.push(footer_muted_span(": "));
        spans.push(footer_muted_span(action.footer_label));
    }

    Line::from(spans)
}

/// Returns one highlighted footer key span.
pub(crate) fn footer_key_span(key: &'static str) -> Span<'static> {
    Span::styled(
        key.to_string(),
        Style::default()
            .fg(style::palette::accent())
            .add_modifier(Modifier::BOLD),
    )
}

/// Returns one muted footer text span.
pub(crate) fn footer_muted_span(text: impl Into<String>) -> Span<'static> {
    Span::styled(
        text.into(),
        Style::default().fg(style::palette::text_muted()),
    )
}

/// Returns the separator between footer help items.
pub(crate) fn footer_separator_span() -> Span<'static> {
    Span::styled(" | ", Style::default().fg(style::palette::text_subtle()))
}

#[cfg(test)]
#[path = "help_format_test.rs"]
mod tests;
