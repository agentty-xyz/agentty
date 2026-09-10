use ag_tui_text::text_util::truncate_with_ellipsis;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::domain::session::Session;
use crate::ui::style::palette;
use crate::ui::{Component, overlay};

/// Minimum popup width sized for the action hint and useful session titles.
const MIN_OVERLAY_WIDTH: u16 = 52;
/// Percentage of the frame width used by the parent selector.
const OVERLAY_WIDTH_PERCENT: u16 = 36;
/// Header, blank separators, and bottom help hint around the parent rows.
const OVERLAY_CHROME_LINE_COUNT: usize = 4;

/// Centered popup used to choose the parent of an existing review session.
pub struct StackAppendParentOverlay<'a> {
    /// Eligible parent sessions in visible list order.
    parent_sessions: &'a [&'a Session],
    /// Currently highlighted parent row.
    selected_parent_index: usize,
}

impl<'a> StackAppendParentOverlay<'a> {
    /// Creates a parent selector for one append-to-stack action.
    pub fn new(parent_sessions: &'a [&'a Session], selected_parent_index: usize) -> Self {
        Self {
            parent_sessions,
            selected_parent_index,
        }
    }

    /// Returns the render lines that fit in the popup around the selection.
    fn lines(&self, label_width: usize, popup_height: u16) -> Vec<Line<'static>> {
        let mut lines = vec![
            Line::from(Span::styled(
                "Choose parent session",
                Style::default()
                    .fg(palette::warning())
                    .add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Center),
            Line::from(""),
        ];

        for row_index in self.visible_parent_range(popup_height) {
            let session = self.parent_sessions[row_index];
            lines.push(self.parent_line(row_index, session, label_width));
        }

        lines.push(Line::from(""));
        lines.push(
            Line::from(Span::styled(
                "j/k: move | Enter: append | q: close",
                Style::default().fg(palette::text_muted()),
            ))
            .alignment(Alignment::Center),
        );

        lines
    }

    /// Returns the candidate range that keeps the selected row visible.
    fn visible_parent_range(&self, popup_height: u16) -> std::ops::Range<usize> {
        let visible_parent_count = usize::from(
            popup_height
                .saturating_sub(overlay::overlay_required_height(OVERLAY_CHROME_LINE_COUNT)),
        )
        .min(self.parent_sessions.len());
        let selected_parent_index = self
            .selected_parent_index
            .min(self.parent_sessions.len().saturating_sub(1));
        let maximum_start_index = self
            .parent_sessions
            .len()
            .saturating_sub(visible_parent_count);
        let start_index = selected_parent_index
            .saturating_sub(visible_parent_count / 2)
            .min(maximum_start_index);

        start_index..start_index.saturating_add(visible_parent_count)
    }

    /// Computes a centered popup whose height follows the candidate count.
    fn popup_area(&self, area: Rect) -> Rect {
        let required_height = overlay::overlay_required_height(
            self.parent_sessions
                .len()
                .saturating_add(OVERLAY_CHROME_LINE_COUNT),
        );

        overlay::centered_popup_area(
            area,
            OVERLAY_WIDTH_PERCENT,
            0,
            MIN_OVERLAY_WIDTH,
            required_height,
        )
    }

    /// Builds one candidate row with the active selection style.
    fn parent_line(
        &self,
        row_index: usize,
        session: &Session,
        label_width: usize,
    ) -> Line<'static> {
        let label = truncate_with_ellipsis(session.display_title(), label_width);
        let row_text = format!(" {label:<label_width$} ");
        let style = if row_index == self.selected_parent_index {
            Style::default()
                .fg(palette::surface_overlay())
                .bg(palette::accent())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette::text())
        };

        Line::from(Span::styled(row_text, style))
    }
}

impl Component for StackAppendParentOverlay<'_> {
    fn render(&self, frame: &mut Frame, area: Rect) {
        let popup_area = self.popup_area(area);
        let label_width = overlay::overlay_content_width(popup_area.width)
            .saturating_sub(2)
            .max(1);
        let paragraph = Paragraph::new(self.lines(label_width, popup_area.height))
            .alignment(Alignment::Left)
            .block(overlay::overlay_block("Append to stack", palette::accent()));

        overlay::clear_popup_area(frame, popup_area);
        frame.render_widget(paragraph, popup_area);
    }
}

#[cfg(test)]
#[path = "stack_append_parent_overlay_test.rs"]
mod tests;
