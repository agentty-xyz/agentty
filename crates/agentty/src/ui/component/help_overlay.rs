use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::presentation::app_mode::HelpContext;
use crate::presentation::help_action::HelpAction;
use crate::ui::style::palette;
use crate::ui::{Component, help_format, overlay};

/// Corner columns around the bottom hint, which carries its own spacing.
const HINT_CHROME_WIDTH: usize = 2;

/// Corner columns plus the spaces the shared overlay frame adds around titles.
const TITLE_CHROME_WIDTH: usize = 4;

/// Columns added by the overlay frame borders and horizontal padding.
const HORIZONTAL_CHROME_WIDTH: usize = 6;

/// Bottom-border shortcut that dismisses the overlay.
const CLOSE_ACTION: HelpAction = HelpAction::new("close", "Esc", "Close");

/// Bottom-border shortcut shown when the shortcut list overflows the popup.
const SCROLL_ACTION: HelpAction = HelpAction::new("scroll", "j/k", "Scroll");

const SCROLL_X_OFFSET: u16 = 0;

/// Centered popup overlay showing keybindings for the current page.
///
/// The popup shrinks to fit its title, shortcut rows, and bottom hint, and
/// only clamps to the frame when the terminal is smaller than that content.
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

    /// Builds the bottom-border hint, adding the scroll cue only when the
    /// shortcut rows overflow the popup height.
    fn bottom_hint(is_overflowing: bool) -> Line<'static> {
        let actions: &[HelpAction] = if is_overflowing {
            &[SCROLL_ACTION, CLOSE_ACTION]
        } else {
            &[CLOSE_ACTION]
        };
        let mut hint = help_format::footer_line(actions);
        hint.spans.insert(0, Span::raw(" "));
        hint.spans.push(Span::raw(" "));

        hint
    }

    /// Returns a centered popup rectangle sized to the widest of the title,
    /// shortcut rows, and bottom hint, clamped to `area`.
    fn popup_area(area: Rect, title: &str, row_width: usize, row_count: usize) -> Rect {
        let required_height = overlay::overlay_required_height(row_count);
        let bottom_hint = Self::bottom_hint(required_height > area.height);
        let title_width = title.len() + TITLE_CHROME_WIDTH;
        let hint_width = bottom_hint.width() + HINT_CHROME_WIDTH;
        let required_width = (row_width + HORIZONTAL_CHROME_WIDTH)
            .max(title_width)
            .max(hint_width);
        let required_width = u16::try_from(required_width).unwrap_or(u16::MAX);

        overlay::centered_popup_area(area, 0, 0, required_width, required_height)
    }
}

impl Component for HelpOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let bindings = self.context.keybindings();
        let title = self.context.title();

        let key_width = bindings
            .iter()
            .map(|binding| binding.key.len())
            .max()
            .unwrap_or(0);

        let row_width = bindings
            .iter()
            .map(|binding| key_width + 2 + binding.popup_label.len())
            .max()
            .unwrap_or(0);

        let popup_area = Self::popup_area(area, title, row_width, bindings.len());

        overlay::clear_popup_area(f, popup_area);

        let content_width = overlay::overlay_content_width(popup_area.width);
        let indent = " ".repeat(content_width.saturating_sub(row_width) / 2);

        let lines: Vec<Line<'_>> = bindings
            .iter()
            .map(|binding| {
                Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(
                        format!("{:>key_width$}", binding.key),
                        Style::default()
                            .fg(palette::accent())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(": ", Style::default().fg(palette::text())),
                    Span::styled(binding.popup_label, Style::default().fg(palette::text())),
                ])
            })
            .collect();

        let is_overflowing = overlay::overlay_required_height(lines.len()) > popup_area.height;
        let block = overlay::overlay_block(title, palette::accent())
            .title_bottom(Self::bottom_hint(is_overflowing).centered());
        let paragraph = Paragraph::new(lines)
            .block(block)
            .scroll((self.scroll_offset, SCROLL_X_OFFSET));

        f.render_widget(paragraph, popup_area);
    }
}

#[cfg(test)]
#[path = "help_overlay_test.rs"]
mod tests;
