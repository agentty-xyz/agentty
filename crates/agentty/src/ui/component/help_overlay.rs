use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::presentation::app_mode::HelpContext;
use crate::ui::style::palette;
use crate::ui::{Component, overlay};

const MIN_OVERLAY_WIDTH: u16 = 30;

const MIN_OVERLAY_HEIGHT: u16 = 10;

/// Popup dimensions for the mode-sensitive keybinding help overlay.
const OVERLAY_DIMENSIONS: overlay::OverlayDimensions =
    overlay::OverlayDimensions::new(40, 60, MIN_OVERLAY_WIDTH, MIN_OVERLAY_HEIGHT);

const SCROLL_X_OFFSET: u16 = 0;

/// Centered popup overlay showing keybindings for the current page.
pub struct HelpOverlay<'a> {
    context: &'a HelpContext,

    scroll_offset: u16,
}

impl<'a> HelpOverlay<'a> {
    /// Creates a help overlay for the given context.
    pub fn new(context: &'a HelpContext) -> Self {
        Self {
            context,
            scroll_offset: 0,
        }
    }

    /// Sets the vertical scroll offset.
    #[must_use]
    pub fn scroll_offset(mut self, offset: u16) -> Self {
        self.scroll_offset = offset;
        self
    }
}

impl Component for HelpOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let popup_area = OVERLAY_DIMENSIONS.centered_popup_area(area);

        overlay::clear_popup_area(f, popup_area);

        let bindings = self.context.keybindings();

        let key_width = bindings
            .iter()
            .map(|binding| binding.key.len())
            .max()
            .unwrap_or(0);

        let max_content_width = bindings
            .iter()
            .map(|binding| 1 + key_width + 2 + binding.popup_label.len())
            .max()
            .unwrap_or(0);

        let content_width = overlay::overlay_content_width(popup_area.width);
        let left_padding = content_width.saturating_sub(max_content_width) / 2;

        let indent = " ".repeat(left_padding);

        let mut lines: Vec<Line<'_>> = Vec::with_capacity(bindings.len());

        for binding in bindings {
            lines.push(Line::from(vec![
                Span::raw(indent.clone()),
                Span::raw(" "),
                Span::styled(
                    format!("{:>key_width$}", binding.key),
                    Style::default()
                        .fg(palette::accent())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(": ", Style::default().fg(palette::text())),
                Span::styled(binding.popup_label, Style::default().fg(palette::text())),
            ]));
        }

        let paragraph = Paragraph::new(lines)
            .block(overlay::overlay_block(
                self.context.title(),
                palette::accent(),
            ))
            .scroll((self.scroll_offset, SCROLL_X_OFFSET));

        f.render_widget(paragraph, popup_area);
    }
}

#[cfg(test)]
#[path = "help_overlay_test.rs"]
mod tests;
