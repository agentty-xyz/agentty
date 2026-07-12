use ag_tui_text::text_util::truncate_with_ellipsis;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::domain::project::ProjectListItem;
use crate::ui::style::palette;
use crate::ui::{Component, overlay};

/// Minimum popup width sized for the help hint, which is wider than typical
/// project labels plus the active-session column.
const MIN_OVERLAY_WIDTH: u16 = 42;
/// Percentage of the frame width used by the switcher popup. Project names are
/// short, so the popup stays narrow and grows only on wide terminals.
const OVERLAY_WIDTH_PERCENT: u16 = 30;
/// Fixed width reserved for the trailing active-session column, sized for the
/// `▶ ` indicator plus a three-digit count.
const ACTIVE_COUNT_WIDTH: usize = 5;
/// Chrome lines around the project rows: header, one blank line above and
/// below the rows, and the bottom help hint.
const OVERLAY_CHROME_LINE_COUNT: usize = 4;

/// Centered popup used to switch the active project from the sessions list.
///
/// Rows are expected in most-recently-opened order; the currently active
/// project is marked with a `* ` prefix, matching the projects table. The
/// trailing column shows only the running session count as `▶ N`, and stays
/// blank for projects without active sessions.
pub struct ProjectSwitcherOverlay<'a> {
    /// Identifier of the currently active project.
    active_project_id: i64,
    /// Project rows in most-recently-opened order.
    project_items: &'a [&'a ProjectListItem],
    /// Currently highlighted row in `project_items`.
    selected_option_index: usize,
}

impl<'a> ProjectSwitcherOverlay<'a> {
    /// Creates a project switcher popup over MRU-ordered project rows.
    pub fn new(
        project_items: &'a [&'a ProjectListItem],
        active_project_id: i64,
        selected_option_index: usize,
    ) -> Self {
        Self {
            active_project_id,
            project_items,
            selected_option_index,
        }
    }

    /// Returns all render lines for this popup.
    ///
    /// The header and bottom help hint rows are centered, while project rows
    /// keep one fixed label column so names and active-session counts align.
    fn lines(&self, label_width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        lines.push(
            Line::from(vec![Span::styled(
                "Switch project",
                Style::default()
                    .fg(palette::warning())
                    .add_modifier(Modifier::BOLD),
            )])
            .alignment(Alignment::Center),
        );
        lines.push(Line::from(""));

        for (row_index, project_item) in self.project_items.iter().enumerate() {
            lines.push(self.project_line(row_index, project_item, label_width));
        }

        lines.push(Line::from(""));
        lines.push(
            Line::from(vec![Span::styled(
                "j/k: move | Enter: switch | q: close",
                Style::default().fg(palette::text_muted()),
            )])
            .alignment(Alignment::Center),
        );

        lines
    }

    /// Computes a centered popup rectangle whose height grows with the
    /// project count and stays clamped to the frame.
    fn popup_area(&self, area: Rect) -> Rect {
        let inner_line_count = self
            .project_items
            .len()
            .saturating_add(OVERLAY_CHROME_LINE_COUNT);
        let required_height = overlay::overlay_required_height(inner_line_count);

        overlay::centered_popup_area(
            area,
            OVERLAY_WIDTH_PERCENT,
            0,
            MIN_OVERLAY_WIDTH,
            required_height,
        )
    }

    /// Builds one project row and applies the active selection style.
    fn project_line(
        &self,
        row_index: usize,
        project_item: &ProjectListItem,
        label_width: usize,
    ) -> Line<'static> {
        let is_active = project_item.project.id == self.active_project_id;
        let marker = if is_active { "* " } else { "  " };
        let label = truncate_with_ellipsis(
            &format!("{marker}{}", project_item.project.display_label()),
            label_width,
        );
        let active_count_label = match project_item.active_session_count {
            0 => String::new(),
            active_session_count => format!("▶ {active_session_count}"),
        };

        if row_index == self.selected_option_index {
            let row_text =
                format!(" {label:<label_width$} {active_count_label:>ACTIVE_COUNT_WIDTH$} ");

            return Line::from(Span::styled(
                row_text,
                Style::default()
                    .fg(palette::surface_overlay())
                    .bg(palette::accent())
                    .add_modifier(Modifier::BOLD),
            ));
        }

        let label_style = if is_active {
            Style::default()
                .fg(palette::accent())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette::text())
        };

        Line::from(vec![
            Span::styled(" ", Style::default().fg(palette::text_subtle())),
            Span::styled(format!("{label:<label_width$}"), label_style),
            Span::styled(
                format!(" {active_count_label:>ACTIVE_COUNT_WIDTH$} "),
                Style::default().fg(palette::warning()),
            ),
        ])
    }
}

impl Component for ProjectSwitcherOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let popup_area = self.popup_area(area);
        let label_width = overlay::overlay_content_width(popup_area.width)
            .saturating_sub(ACTIVE_COUNT_WIDTH + 3)
            .max(1);
        let paragraph = Paragraph::new(self.lines(label_width))
            .alignment(Alignment::Left)
            .block(overlay::overlay_block("Projects", palette::accent()));

        overlay::clear_popup_area(f, popup_area);
        f.render_widget(paragraph, popup_area);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::domain::project::Project;

    /// Builds one project row fixture for switcher render tests.
    fn project_list_item_fixture(
        id: i64,
        name: &str,
        active_session_count: u32,
    ) -> ProjectListItem {
        ProjectListItem {
            active_session_count,
            input_tokens: 0,
            last_session_updated_at: None,
            output_tokens: 0,
            project: Project {
                created_at: 0,
                display_name: Some(name.to_string()),
                git_branch: Some("main".to_string()),
                id,
                is_favorite: false,
                last_opened_at: None,
                path: PathBuf::from(format!("/tmp/{name}")),
                updated_at: 0,
            },
            session_count: 7,
        }
    }

    /// Flattens a rendered test buffer into a plain string for text checks.
    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn test_project_switcher_overlay_popup_area_grows_with_project_count() {
        // Arrange
        let area = ratatui::layout::Rect::new(0, 0, 80, 24);
        let first_project = project_list_item_fixture(1, "agentty", 3);
        let second_project = project_list_item_fixture(2, "service", 1);
        let project_items = vec![&first_project, &second_project];
        let overlay = ProjectSwitcherOverlay::new(&project_items, 1, 0);

        // Act
        let popup_area = overlay.popup_area(area);

        // Assert
        assert_eq!(popup_area.width, 42);
        assert_eq!(popup_area.height, 10);
        assert_eq!(popup_area.x, 19);
        assert_eq!(popup_area.y, 7);
    }

    #[test]
    fn test_project_switcher_overlay_render_shows_projects_and_hints() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let first_project = project_list_item_fixture(1, "agentty", 3);
        let second_project = project_list_item_fixture(2, "service", 1);
        let project_items = vec![&first_project, &second_project];
        let overlay = ProjectSwitcherOverlay::new(&project_items, 1, 0);

        // Act
        terminal
            .draw(|frame| {
                Component::render(&overlay, frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Switch project"));
        assert!(text.contains("* agentty"));
        assert!(text.contains("service"));
        assert!(text.contains("▶ 3"));
        assert!(text.contains("▶ 1"));
        assert!(text.contains("j/k: move | Enter: switch | q: close"));
    }

    #[test]
    fn test_project_switcher_overlay_render_aligns_project_labels() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let first_project = project_list_item_fixture(1, "agentty", 3);
        let second_project = project_list_item_fixture(2, "service", 1);
        let project_items = vec![&first_project, &second_project];
        let overlay = ProjectSwitcherOverlay::new(&project_items, 1, 0);

        // Act
        terminal
            .draw(|frame| {
                Component::render(&overlay, frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let active_label_column =
            text_start_column(buffer, "agentty").expect("active project label should render");
        let inactive_label_column =
            text_start_column(buffer, "service").expect("inactive project label should render");
        assert_eq!(active_label_column, inactive_label_column);
    }

    /// Finds the first column where `needle` starts in the test buffer.
    fn text_start_column(buffer: &ratatui::buffer::Buffer, needle: &str) -> Option<u16> {
        let needle_symbols = needle
            .chars()
            .map(|character| character.to_string())
            .collect::<Vec<_>>();
        let width = usize::from(buffer.area.width.max(1));

        for row in buffer.content().chunks(width) {
            for (column_index, window) in row.windows(needle_symbols.len()).enumerate() {
                let window_matches = window
                    .iter()
                    .zip(&needle_symbols)
                    .all(|(cell, symbol)| cell.symbol() == symbol);

                if window_matches {
                    return u16::try_from(column_index).ok();
                }
            }
        }

        None
    }

    #[test]
    fn test_project_switcher_overlay_lines_highlight_selected_row() {
        // Arrange
        let first_project = project_list_item_fixture(1, "agentty", 3);
        let second_project = project_list_item_fixture(2, "service", 1);
        let project_items = vec![&first_project, &second_project];
        let overlay = ProjectSwitcherOverlay::new(&project_items, 1, 1);

        // Act
        let lines = overlay.lines(20);
        let selected_line = &lines[3];
        let unselected_line = &lines[2];

        // Assert
        assert!(
            selected_line
                .spans
                .iter()
                .all(|span| span.style.bg == Some(palette::accent()))
        );
        assert!(
            unselected_line
                .spans
                .iter()
                .all(|span| span.style.bg != Some(palette::accent()))
        );
    }

    #[test]
    fn test_project_switcher_overlay_lines_show_only_running_session_counts() {
        // Arrange
        let busy_project = project_list_item_fixture(1, "agentty", 2);
        let idle_project = project_list_item_fixture(2, "service", 0);
        let project_items = vec![&busy_project, &idle_project];
        let overlay = ProjectSwitcherOverlay::new(&project_items, 1, 0);

        // Act
        let lines = overlay.lines(20);
        let busy_row_text: String = lines[2]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        let idle_row_text: String = lines[3]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        // Assert
        assert!(busy_row_text.contains("▶ 2"));
        assert_eq!(idle_row_text.trim(), "service");
    }

    #[test]
    fn test_project_switcher_overlay_lines_mark_active_project() {
        // Arrange
        let first_project = project_list_item_fixture(1, "agentty", 3);
        let second_project = project_list_item_fixture(2, "service", 1);
        let project_items = vec![&first_project, &second_project];
        let overlay = ProjectSwitcherOverlay::new(&project_items, 2, 0);

        // Act
        let lines = overlay.lines(20);
        let active_row_text: String = lines[3]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        let inactive_row_text: String = lines[2]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        // Assert
        assert!(active_row_text.contains("* service"));
        assert!(!inactive_row_text.contains('*'));
    }

    #[test]
    fn test_project_switcher_overlay_lines_center_header_and_help_text() {
        // Arrange
        let first_project = project_list_item_fixture(1, "agentty", 0);
        let project_items = vec![&first_project];
        let overlay = ProjectSwitcherOverlay::new(&project_items, 1, 0);

        // Act
        let lines = overlay.lines(20);
        let header_line = lines.first().expect("overlay should include a header line");
        let help_line = lines.last().expect("overlay should include a help line");

        // Assert
        assert_eq!(header_line.alignment, Some(Alignment::Center));
        assert_eq!(help_line.alignment, Some(Alignment::Center));
    }
}
