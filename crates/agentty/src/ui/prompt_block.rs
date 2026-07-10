//! Shared transcript prompt-block layout constants.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::presentation::style;
use crate::ui::text_util;

/// Visible prefix for the first row of a user prompt in transcript output.
pub(crate) const USER_PROMPT_PREFIX: &str = " › ";

/// Reserved trailing cells between prompt text and the right edge.
pub(crate) const USER_PROMPT_RIGHT_GUTTER_WIDTH: usize = 1;

/// Returns visible padding for continuation rows of a user prompt in
/// transcript output.
pub(crate) fn user_prompt_continuation_prefix() -> String {
    " ".repeat(USER_PROMPT_PREFIX.chars().count())
}

/// Returns one full-width blank row using the user-prompt background.
pub(crate) fn user_prompt_padding_line(width: usize) -> Line<'static> {
    Line::styled(" ".repeat(width), user_prompt_content_style())
}

/// Adds the transcript prompt marker to one rendered markdown line.
pub(crate) fn user_prompt_markdown_line(
    rendered_spans: impl IntoIterator<Item = Span<'static>>,
    prefix: &str,
    prefix_style: Style,
    width: usize,
) -> Line<'static> {
    let content_style = user_prompt_content_style();
    let mut spans = vec![Span::styled(prefix.to_string(), prefix_style)];
    spans.extend(rendered_spans.into_iter().map(user_prompt_content_span));

    let mut line = Line::from(spans);
    let line_width = line.width();
    if line_width > width {
        line.spans =
            text_util::truncate_spans_with_ellipsis(std::mem::take(&mut line.spans), width)
                .into_iter()
                .map(user_prompt_content_span)
                .collect();
    } else if line_width < width {
        line.spans
            .push(Span::styled(" ".repeat(width - line_width), content_style));
    }

    line
}

/// Preserves markdown foreground/modifier styling while applying the
/// prompt-row background to rendered user prompt content.
pub(crate) fn user_prompt_content_span(mut span: Span<'static>) -> Span<'static> {
    let content_style = user_prompt_content_style();
    if span.style.fg.is_none() {
        span.style.fg = content_style.fg;
    }
    if span.style.bg.is_none() {
        span.style.bg = content_style.bg;
    }

    span
}

/// Returns the style for the visible user prompt marker.
pub(crate) fn user_prompt_prefix_style() -> Style {
    Style::default()
        .fg(style::palette::accent())
        .bg(style::palette::surface_prompt())
        .add_modifier(Modifier::BOLD)
}

/// Returns the base style for user prompt content rows.
pub(crate) fn user_prompt_content_style() -> Style {
    Style::default()
        .fg(style::palette::text())
        .bg(style::palette::surface_prompt())
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;
    use crate::domain::theme::ColorTheme;

    #[test]
    fn test_user_prompt_styles_use_prompt_surface_background() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);

        // Act
        let prefix_style = user_prompt_prefix_style();
        let content_style = user_prompt_content_style();

        // Assert
        assert_eq!(prefix_style.bg, Some(style::palette::surface_prompt()));
        assert_eq!(content_style.bg, Some(style::palette::surface_prompt()));
    }

    #[test]
    fn test_current_theme_prompt_surface_is_terminal_scheme_independent() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);

        // Act
        let prompt_surface = style::palette::surface_prompt();

        // Assert
        assert!(
            matches!(prompt_surface, Color::Rgb(..)),
            "prompt blocks must pin an RGB surface so terminal ANSI schemes cannot remap it to a \
             light color",
        );
    }
}
