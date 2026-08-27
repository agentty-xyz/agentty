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
mod tests {
    use super::*;
    use crate::domain::session::Status;

    #[test]
    fn test_stack_append_parent_overlay_renders_candidates_and_hint() {
        // Arrange
        let first_session =
            crate::test_support::titled_session_fixture("first-session", Status::Review);
        let mut second_session =
            crate::test_support::titled_session_fixture("second-session", Status::AgentReview);
        second_session.title = Some("Second parent".to_string());
        let parent_sessions = vec![&first_session, &second_session];
        let overlay = StackAppendParentOverlay::new(&parent_sessions, 1);
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| Component::render(&overlay, frame, frame.area()))
            .expect("failed to draw");

        // Assert
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(text.contains("Append to stack"));
        assert!(text.contains("Choose parent session"));
        assert!(text.contains("Second parent"));
        assert!(text.contains("Enter: append"));
        let selected_line = &overlay.lines(30, 10)[3];
        assert!(
            selected_line
                .spans
                .iter()
                .any(|span| span.style.bg == Some(palette::accent()))
        );
    }

    #[test]
    fn test_stack_append_parent_overlay_height_grows_with_candidates() {
        // Arrange
        let first_session =
            crate::test_support::titled_session_fixture("first-session", Status::Review);
        let second_session =
            crate::test_support::titled_session_fixture("second-session", Status::Review);
        let parent_sessions = vec![&first_session, &second_session];
        let overlay = StackAppendParentOverlay::new(&parent_sessions, 0);

        // Act
        let popup_area = overlay.popup_area(Rect::new(0, 0, 80, 24));

        // Assert
        assert_eq!(popup_area.width, 52);
        assert_eq!(popup_area.height, 10);
    }

    #[test]
    fn test_stack_append_parent_overlay_windows_over_height_candidates_around_selection() {
        // Arrange
        let sessions = (0..10)
            .map(|index| {
                let mut session = crate::test_support::titled_session_fixture(
                    &format!("parent-{index}"),
                    Status::Review,
                );
                session.title = Some(format!("Parent {index}"));

                session
            })
            .collect::<Vec<_>>();
        let parent_sessions = sessions.iter().collect::<Vec<_>>();
        let overlay = StackAppendParentOverlay::new(&parent_sessions, 8);
        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| Component::render(&overlay, frame, frame.area()))
            .expect("failed to draw");
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();

        // Assert
        assert!(text.contains("Parent 8"));
        assert!(!text.contains("Parent 0"));
        assert!(text.contains("Enter: append"));
    }
}
