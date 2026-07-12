use ag_forge::AssignedIssue;
use ag_tui_text::text_util::inline_text;
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use crate::app::AssignedIssueState;
use crate::presentation::help_action;
use crate::ui::{Page, layout, style};

/// Horizontal spacing between assigned-issue table columns.
const TABLE_COLUMN_SPACING: u16 = 2;
/// Maximum assigned issues returned by the GitHub CLI query.
const ASSIGNED_ISSUE_DISPLAY_LIMIT: usize = 100;

/// List page for open GitHub issues assigned to the authenticated user.
pub struct IssueListPage<'a> {
    assigned_issues: &'a AssignedIssueState,
    selected_issue_index: Option<usize>,
    table_state: &'a mut TableState,
}

impl<'a> IssueListPage<'a> {
    /// Creates an assigned-issue page from the active project's cache.
    pub fn new(
        assigned_issues: &'a AssignedIssueState,
        selected_issue_index: Option<usize>,
        table_state: &'a mut TableState,
    ) -> Self {
        Self {
            assigned_issues,
            selected_issue_index,
            table_state,
        }
    }
}

impl Page for IssueListPage<'_> {
    fn render(&mut self, frame: &mut Frame, area: Rect) {
        let areas = layout::tab_page_areas(area);

        match self.assigned_issues {
            AssignedIssueState::Loaded { items, .. } if !items.is_empty() => render_issue_table(
                frame,
                areas.main_area,
                items,
                self.selected_issue_index,
                self.table_state,
            ),
            AssignedIssueState::Loaded { .. } => render_message(
                frame,
                areas.main_area,
                "No open GitHub issues are assigned to you.",
            ),
            AssignedIssueState::Failed { message, .. } => render_message(
                frame,
                areas.main_area,
                &format!("Failed to load assigned issues: {message}"),
            ),
            AssignedIssueState::Idle | AssignedIssueState::Loading { .. } => {
                render_message(frame, areas.main_area, "Loading assigned GitHub issues...");
            }
        }

        let mut footer = crate::ui::help_format::footer_line(&help_action::issue_actions());
        if matches!(self.assigned_issues, AssignedIssueState::Loaded { items, .. } if items.len() >= ASSIGNED_ISSUE_DISPLAY_LIMIT)
        {
            footer
                .spans
                .push(crate::ui::help_format::footer_separator_span());
            footer
                .spans
                .push(crate::ui::help_format::footer_muted_span(format!(
                    "showing first {ASSIGNED_ISSUE_DISPLAY_LIMIT}"
                )));
        }
        frame.render_widget(Paragraph::new(footer), areas.footer_area);
    }
}

/// Renders the populated assigned-issue table.
fn render_issue_table(
    frame: &mut Frame,
    area: Rect,
    items: &[AssignedIssue],
    selected_issue_index: Option<usize>,
    table_state: &mut TableState,
) {
    let header = Row::new(["Issue", "Repository", "Updated"])
        .style(
            Style::default()
                .bg(style::palette::surface())
                .fg(style::palette::text_muted())
                .add_modifier(Modifier::BOLD),
        )
        .height(1);
    let constraints = [
        Constraint::Fill(1),
        Constraint::Length(28),
        Constraint::Length(12),
    ];
    let rows = items.iter().map(|issue| {
        let issue_label = format!("{} {}", issue.display_id, issue.title);
        let issue_text = inline_text(&issue_label);

        Row::new([
            Cell::from(issue_text),
            Cell::from(issue.repository.as_str()),
            Cell::from(issue.updated_at.as_deref().map_or("", issue_updated_date)),
        ])
    });
    table_state.select(selected_issue_index);
    let table = Table::new(rows, constraints)
        .column_spacing(TABLE_COLUMN_SPACING)
        .header(header)
        .block(issue_block())
        .row_highlight_style(Style::default().bg(style::palette::surface_selection()));

    frame.render_stateful_widget(table, area, table_state);
}

/// Returns the calendar-date prefix from a provider timestamp when available.
fn issue_updated_date(updated_at: &str) -> &str {
    updated_at.get(..10).unwrap_or(updated_at)
}

/// Renders one state message inside the issue-list frame.
fn render_message(frame: &mut Frame, area: Rect, message: &str) {
    frame.render_widget(
        Paragraph::new(message.to_string())
            .block(issue_block())
            .style(Style::default().fg(style::palette::text_muted())),
        area,
    );
}

/// Builds the assigned-issue list block.
fn issue_block() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title("Assigned GitHub Issues")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_updated_date_uses_iso_calendar_prefix() {
        // Arrange
        let updated_at = "2026-07-09T18:30:00Z";

        // Act
        let date = issue_updated_date(updated_at);

        // Assert
        assert_eq!(date, "2026-07-09");
    }
}
