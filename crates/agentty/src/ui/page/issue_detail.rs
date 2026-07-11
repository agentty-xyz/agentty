use ag_forge::{AssignedIssue, IssueDetail};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::presentation::help_action;
use crate::ui::{Page, help_format, layout, markdown, style};

/// Page renderer for one selected GitHub issue and its base details.
pub struct IssueDetailPage<'a> {
    detail: Option<&'a IssueDetail>,
    error: Option<&'a str>,
    issue: &'a AssignedIssue,
    markdown_render_cache: &'a markdown::MarkdownRenderCache,
    scroll_offset: u16,
}

impl<'a> IssueDetailPage<'a> {
    /// Creates an issue-detail renderer for a selected list row and its
    /// asynchronous detail state.
    pub fn new(
        issue: &'a AssignedIssue,
        detail: Option<&'a IssueDetail>,
        error: Option<&'a str>,
        markdown_render_cache: &'a markdown::MarkdownRenderCache,
        scroll_offset: u16,
    ) -> Self {
        Self {
            detail,
            error,
            issue,
            markdown_render_cache,
            scroll_offset,
        }
    }
}

impl Page for IssueDetailPage<'_> {
    fn render(&mut self, frame: &mut Frame, area: Rect) {
        let areas = layout::tab_page_areas(area);
        let content_area = issue_detail_content_area(area, self.issue);
        let paragraph = Paragraph::new(issue_detail_lines(
            self.issue,
            self.detail,
            self.error,
            self.markdown_render_cache,
            usize::from(content_area.width),
        ))
        .block(issue_detail_block(self.issue))
        .style(Style::default().fg(style::palette::text()))
        .scroll((self.scroll_offset, 0));

        frame.render_widget(paragraph, areas.main_area);
        frame.render_widget(
            Paragraph::new(help_format::footer_line(
                &help_action::issue_detail_actions(),
            )),
            areas.footer_area,
        );
    }
}

/// Returns the largest valid vertical scroll offset for an issue detail page.
pub(crate) fn issue_detail_max_scroll_offset(
    issue: &AssignedIssue,
    detail: Option<&IssueDetail>,
    error: Option<&str>,
    area: Rect,
    markdown_render_cache: &markdown::MarkdownRenderCache,
) -> u16 {
    let content_area = issue_detail_content_area(area, issue);
    let viewport_height = content_area.height;
    if viewport_height == 0 {
        return 0;
    }

    let line_count = issue_detail_lines(
        issue,
        detail,
        error,
        markdown_render_cache,
        usize::from(content_area.width),
    )
    .len();

    u16::try_from(line_count.saturating_sub(usize::from(viewport_height))).unwrap_or(u16::MAX)
}

/// Builds visible metadata and markdown description lines without comments.
fn issue_detail_lines(
    issue: &AssignedIssue,
    detail: Option<&IssueDetail>,
    error: Option<&str>,
    markdown_render_cache: &markdown::MarkdownRenderCache,
    width: usize,
) -> Vec<Line<'static>> {
    let Some(detail) = detail else {
        return vec![Line::from(
            error.unwrap_or("Loading issue details...").to_string(),
        )];
    };
    if issue.display_id != detail.display_id {
        return vec![Line::from(
            "Issue details do not match the selected issue. Return to the list and reopen it.",
        )];
    }

    let assignees = list_text(&detail.assignees);
    let labels = list_text(&detail.labels);
    let created_at = timestamp_text(detail.created_at.as_deref());
    let updated_at = timestamp_text(detail.updated_at.as_deref());
    let description = detail
        .body
        .as_deref()
        .map(str::trim)
        .filter(|body| !body.is_empty())
        .unwrap_or("No description provided.");
    let mut lines = vec![
        field_line("Title", &detail.title),
        field_line("State", &detail.state),
        field_line("Author", &detail.author),
        field_line("Assignees", &assignees),
        field_line("Labels", &labels),
        field_line("Created", created_at),
        field_line("Updated", updated_at),
        field_line("URL", &detail.web_url),
        Line::from(""),
        section_label("Description"),
    ];
    lines.extend(
        markdown_render_cache
            .render(description, width)
            .iter()
            .cloned(),
    );

    lines
}

/// Formats one metadata field as a bold label followed by its value.
fn field_line(label: &'static str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default()
                .fg(style::palette::text_muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(value.to_string()),
    ])
}

/// Formats a section label for the issue description.
fn section_label(label: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        label,
        Style::default()
            .fg(style::palette::text_muted())
            .add_modifier(Modifier::BOLD),
    ))
}

/// Formats a provider string collection or an explicit empty-state label.
fn list_text(items: &[String]) -> String {
    if items.is_empty() {
        "None".to_string()
    } else {
        items.join(", ")
    }
}

/// Returns the first 10 bytes of an ISO 8601 timestamp, or `"Unknown"` when
/// the value is absent or shorter than 10 bytes.
fn timestamp_text(timestamp: Option<&str>) -> &str {
    timestamp
        .and_then(|value| value.get(..10))
        .unwrap_or("Unknown")
}

/// Returns the inner content area of the selected issue detail frame.
fn issue_detail_content_area(area: Rect, issue: &AssignedIssue) -> Rect {
    let areas = layout::tab_page_areas(area);

    issue_detail_block(issue).inner(areas.main_area)
}

/// Builds the selected issue detail block.
fn issue_detail_block(issue: &AssignedIssue) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(format!("Issue {} · {}", issue.display_id, issue.repository))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_text_formats_empty_and_populated_values() {
        // Arrange
        let populated = vec!["bug".to_string(), "ui".to_string()];

        // Act
        let empty_text = list_text(&[]);
        let populated_text = list_text(&populated);

        // Assert
        assert_eq!(empty_text, "None");
        assert_eq!(populated_text, "bug, ui");
    }

    #[test]
    fn test_issue_detail_lines_rejects_mismatched_issue_details() {
        // Arrange
        let issue = assigned_issue();
        let mut detail = issue_detail();
        detail.display_id = "#125".to_string();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();

        // Act
        let lines = issue_detail_lines(&issue, Some(&detail), None, &markdown_render_cache, 80);

        // Assert
        assert_eq!(lines.len(), 1);
        assert!(lines[0].to_string().contains("do not match"));
    }

    #[test]
    fn test_timestamp_text_handles_missing_short_and_iso_values() {
        // Arrange
        let iso_timestamp = Some("2026-07-12T07:04:38Z");
        let short_timestamp = Some("2026-07");

        // Act
        let iso_date = timestamp_text(iso_timestamp);
        let short_date = timestamp_text(short_timestamp);
        let missing_date = timestamp_text(None);

        // Assert
        assert_eq!(iso_date, "2026-07-12");
        assert_eq!(short_date, "Unknown");
        assert_eq!(missing_date, "Unknown");
    }

    #[test]
    fn test_issue_detail_content_area_excludes_footer_and_border() {
        // Arrange
        let area = Rect::new(0, 0, 80, 20);
        let issue = assigned_issue();

        // Act
        let content_area = issue_detail_content_area(area, &issue);

        // Assert
        assert_eq!(content_area, Rect::new(2, 1, 76, 16));
    }

    #[test]
    fn test_issue_detail_max_scroll_offset_uses_inner_viewport_height() {
        // Arrange
        let area = Rect::new(0, 0, 40, 8);
        let issue = assigned_issue();
        let detail = issue_detail();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();

        // Act
        let max_scroll_offset = issue_detail_max_scroll_offset(
            &issue,
            Some(&detail),
            None,
            area,
            &markdown_render_cache,
        );

        // Assert
        assert_eq!(max_scroll_offset, 7);
    }

    /// Builds one assigned-issue row for detail layout tests.
    fn assigned_issue() -> AssignedIssue {
        AssignedIssue {
            display_id: "#124".to_string(),
            repository: "agentty-xyz/agentty".to_string(),
            title: "Keep issue details reachable".to_string(),
            updated_at: None,
            web_url: "https://github.com/agentty-xyz/agentty/issues/124".to_string(),
        }
    }

    /// Builds one loaded issue detail for viewport tests.
    fn issue_detail() -> IssueDetail {
        IssueDetail {
            assignees: Vec::new(),
            author: "octocat".to_string(),
            body: Some("One description line.".to_string()),
            created_at: None,
            display_id: "#124".to_string(),
            labels: Vec::new(),
            repository: "agentty-xyz/agentty".to_string(),
            state: "OPEN".to_string(),
            title: "Keep issue details reachable".to_string(),
            updated_at: None,
            web_url: "https://github.com/agentty-xyz/agentty/issues/124".to_string(),
        }
    }
}
