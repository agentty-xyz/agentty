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
    let table_rows = issue_table_rows(items);
    let rows = table_rows.iter().map(IssueTableRow::render);
    table_state.select(selected_render_row(&table_rows, selected_issue_index));
    let table = Table::new(rows, constraints)
        .column_spacing(TABLE_COLUMN_SPACING)
        .header(header)
        .block(issue_block())
        .row_highlight_style(Style::default().bg(style::palette::surface_selection()));

    frame.render_stateful_widget(table, area, table_state);
}

/// Intermediate row model shared by grouped rendering and selection mapping.
enum IssueTableRow<'a> {
    /// Non-selectable issue group heading.
    Section(&'static str),
    /// Selectable assigned-issue row.
    Issue {
        /// Issue rendered on this table row.
        item: &'a AssignedIssue,
        /// Original issue index in the loaded list.
        item_index: usize,
    },
}

impl IssueTableRow<'_> {
    /// Converts this row model into a Ratatui table row.
    fn render(&self) -> Row<'_> {
        match self {
            Self::Section(label) => Row::new([
                Cell::from(*label).style(Style::default().fg(style::palette::accent())),
                Cell::from(""),
                Cell::from(""),
            ]),
            Self::Issue { item, .. } => {
                let issue_label = format!("{} {}", item.display_id, item.title);
                let issue_text = inline_text(&issue_label);

                Row::new([
                    Cell::from(issue_text),
                    Cell::from(item.repository.as_str()),
                    Cell::from(item.updated_at.as_deref().map_or("", issue_updated_date)),
                ])
            }
        }
    }
}

/// Builds the non-empty assigned-issue group with global indexes for
/// navigation.
fn issue_table_rows(items: &[AssignedIssue]) -> Vec<IssueTableRow<'_>> {
    let mut rows = Vec::with_capacity(items.len() + 1);
    if items.is_empty() {
        return rows;
    }

    rows.push(IssueTableRow::Section("Assigned to you"));
    rows.extend(
        items
            .iter()
            .enumerate()
            .map(|(item_index, item)| IssueTableRow::Issue { item, item_index }),
    );

    rows
}

/// Maps an issue selection to its rendered row after group headings are added.
fn selected_render_row(
    rows: &[IssueTableRow<'_>],
    selected_issue_index: Option<usize>,
) -> Option<usize> {
    let selected_issue_index = selected_issue_index?;

    rows.iter().position(|row| {
        matches!(
            row,
            IssueTableRow::Issue { item_index, .. } if *item_index == selected_issue_index
        )
    })
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

/// Builds the issue-list block.
fn issue_block() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title("Issues")
        .border_style(style::border_style())
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::domain::theme::ColorTheme;

    #[test]
    fn test_render_loaded_issues_shows_title_and_non_empty_group() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let backend = TestBackend::new(100, 10);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
        let state = AssignedIssueState::Loaded {
            items: vec![assigned_issue("#124", "Keep issue list compact")],
            project_id: 1,
        };

        // Act
        terminal
            .draw(|frame| {
                IssueListPage::new(&state, Some(0), &mut table_state).render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<Vec<_>>()
            .join("");
        assert!(text.contains("Issues"));
        assert!(text.contains("Assigned to you"));
        assert!(text.contains("#124 Keep issue list compact"));
    }

    #[test]
    fn test_selected_render_row_skips_group_headings() {
        // Arrange
        let items = vec![
            assigned_issue("#124", "First issue"),
            assigned_issue("#125", "Second issue"),
        ];
        let rows = issue_table_rows(&items);

        // Act
        let first_row = selected_render_row(&rows, Some(0));
        let second_row = selected_render_row(&rows, Some(1));
        let out_of_range_row = selected_render_row(&rows, Some(2));

        // Assert
        assert_eq!(first_row, Some(1));
        assert_eq!(second_row, Some(2));
        assert_eq!(out_of_range_row, None);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn test_issue_updated_date_uses_iso_calendar_prefix() {
        // Arrange
        let updated_at = "2026-07-09T18:30:00Z";

        // Act
        let date = issue_updated_date(updated_at);

        // Assert
        assert_eq!(date, "2026-07-09");
    }

    /// Builds one assigned-issue fixture for render tests.
    fn assigned_issue(display_id: &str, title: &str) -> AssignedIssue {
        AssignedIssue {
            display_id: display_id.to_string(),
            repository: "agentty-xyz/agentty".to_string(),
            title: title.to_string(),
            updated_at: Some("2026-07-09T18:30:00Z".to_string()),
            web_url: format!("https://example.com/{display_id}"),
        }
    }
}
