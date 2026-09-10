use ag_tui_text::text_util::truncate_with_ellipsis;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::ui::style::palette;
use crate::ui::{Component, overlay};

const MIN_OVERLAY_HEIGHT: u16 = 9;
const MIN_OVERLAY_WIDTH: u16 = 50;
/// Popup dimensions for configured launch-configuration selection.
const OVERLAY_DIMENSIONS: overlay::OverlayDimensions =
    overlay::OverlayDimensions::new(62, 38, MIN_OVERLAY_WIDTH, MIN_OVERLAY_HEIGHT);

/// Centered popup that allows selecting one configured launch configuration.
pub struct LaunchConfigurationOverlay<'a> {
    commands: &'a [String],
    selected_command_index: usize,
}

impl<'a> LaunchConfigurationOverlay<'a> {
    /// Creates a launch-configuration selector popup from configured command
    /// values.
    pub fn new(commands: &'a [String]) -> Self {
        Self {
            commands,
            selected_command_index: 0,
        }
    }

    /// Sets which command row is currently highlighted.
    #[must_use]
    pub fn selected_command_index(mut self, selected_command_index: usize) -> Self {
        self.selected_command_index = selected_command_index;
        self
    }

    /// Returns all render lines for this popup.
    ///
    /// The header and bottom help hint rows are centered, while the selected
    /// command row is emphasized by background color only (no prefix marker
    /// glyph).
    fn lines(&self, command_width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        lines.push(
            Line::from(vec![Span::styled(
                "Select launch configuration",
                Style::default()
                    .fg(palette::warning())
                    .add_modifier(Modifier::BOLD),
            )])
            .alignment(Alignment::Center),
        );
        lines.push(Line::from(""));

        for (index, command) in self.commands.iter().enumerate() {
            let is_selected = index == self.selected_command_index;
            let command_label = truncate_with_ellipsis(command, command_width);

            let line = if is_selected {
                let selected_label = format!(" {command_label:<command_width$}");
                Line::from(Span::styled(
                    selected_label,
                    Style::default()
                        .fg(palette::surface_overlay())
                        .bg(palette::accent())
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(vec![
                    Span::styled(" ", Style::default().fg(palette::text_subtle())),
                    Span::styled(command_label, Style::default().fg(palette::text())),
                ])
            };

            lines.push(line);
        }

        lines.push(Line::from(""));
        lines.push(
            Line::from(vec![Span::styled(
                "j/k: move | Enter: open | Esc: cancel",
                Style::default().fg(palette::text_muted()),
            )])
            .alignment(Alignment::Center),
        );

        lines
    }
}

impl Component for LaunchConfigurationOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let popup_area = OVERLAY_DIMENSIONS.centered_popup_area(area);
        let command_width = overlay::overlay_content_width(popup_area.width)
            .saturating_sub(1)
            .max(1);
        let lines = self.lines(command_width);

        let paragraph = Paragraph::new(lines)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true })
            .block(overlay::overlay_block(
                "Launch Configuration",
                palette::accent(),
            ));

        overlay::clear_popup_area(f, popup_area);
        f.render_widget(paragraph, popup_area);
    }
}

#[cfg(test)]
#[path = "launch_configuration_overlay_test.rs"]
mod tests;
