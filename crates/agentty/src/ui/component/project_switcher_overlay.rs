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
#[path = "project_switcher_overlay_test.rs"]
mod tests;
