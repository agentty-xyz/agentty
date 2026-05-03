use ag_forge::{RequestedReview, RequestedReviewAudience};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::app::RequestedReviewState;
use crate::ui::input_layout::first_table_column_width;
use crate::ui::state::help_action;
use crate::ui::text_util::{inline_text, truncate_spans_with_ellipsis};
use crate::ui::{Page, layout, style};

/// Horizontal spacing between requested-review table columns.
const TABLE_COLUMN_SPACING: u16 = 2;
/// Requested-review provider row cap surfaced by the footer when reached.
const REQUESTED_REVIEW_DISPLAY_LIMIT: usize = 100;
/// Page renderer for PRs and MRs requesting the current user's review.
pub struct ReviewListPage<'a> {
    /// Current requested-review cache snapshot to render.
    requested_reviews: &'a RequestedReviewState,
}

impl<'a> ReviewListPage<'a> {
    /// Creates a requested-review list renderer from the current app cache.
    pub fn new(requested_reviews: &'a RequestedReviewState) -> Self {
        Self { requested_reviews }
    }
}

impl Page for ReviewListPage<'_> {
    /// Renders the requested-review table plus loading, empty, and error
    /// states with compact tab-page spacing.
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let areas = layout::tab_page_areas(area);
        let show_limit_hint = is_at_display_limit(self.requested_reviews);

        match self.requested_reviews {
            RequestedReviewState::Loaded { items, .. } if !items.is_empty() => {
                render_review_table(f, areas.main_area, items);
            }
            RequestedReviewState::Loaded { .. } => {
                render_message(
                    f,
                    areas.main_area,
                    "No PRs or MRs currently request your review.",
                );
            }
            RequestedReviewState::Failed { message, .. } => {
                render_message(
                    f,
                    areas.main_area,
                    &format!("Failed to load requested reviews: {message}"),
                );
            }
            RequestedReviewState::Idle | RequestedReviewState::Loading { .. } => {
                render_message(f, areas.main_area, "Loading requested reviews...");
            }
        }

        let help = Paragraph::new(review_footer_line(show_limit_hint));
        f.render_widget(help, areas.footer_area);
    }
}

/// Renders the populated requested-review table.
fn render_review_table(f: &mut Frame, area: Rect, items: &[RequestedReview]) {
    let header_style = Style::default()
        .bg(style::palette::surface())
        .fg(style::palette::text_muted())
        .add_modifier(Modifier::BOLD);
    let header_cells = ["Review", "Repository", "Status", "Updated"]
        .iter()
        .map(|header| Cell::from(*header));
    let header = Row::new(header_cells)
        .style(header_style)
        .height(1)
        .bottom_margin(1);

    let block = review_block();
    let constraints = [
        Constraint::Fill(1),
        repository_column_width(items),
        status_column_width(items),
        Constraint::Length(12),
    ];
    let title_column_width = first_table_column_width(
        block.inner(area).width,
        &constraints,
        TABLE_COLUMN_SPACING,
        0,
    );
    let rows = review_rows(items, title_column_width);
    let table = Table::new(rows, constraints)
        .column_spacing(TABLE_COLUMN_SPACING)
        .header(header)
        .block(block);

    f.render_widget(table, area);
}

/// Renders one centered state message inside the review list frame.
fn render_message(f: &mut Frame, area: Rect, message: &str) {
    let paragraph = Paragraph::new(message.to_string())
        .block(review_block())
        .style(Style::default().fg(style::palette::text_muted()));

    f.render_widget(paragraph, area);
}

/// Returns whether the loaded list reached the provider row cap rendered in
/// the footer hint.
fn is_at_display_limit(requested_reviews: &RequestedReviewState) -> bool {
    matches!(
        requested_reviews,
        RequestedReviewState::Loaded { items, .. } if items.len() >= REQUESTED_REVIEW_DISPLAY_LIMIT
    )
}

/// Builds the review footer, including read-only and truncation hints.
fn review_footer_line(show_limit_hint: bool) -> Line<'static> {
    let mut line = help_action::footer_line(&help_action::review_actions());
    line.spans.push(help_action::footer_separator_span());
    line.spans
        .push(help_action::footer_muted_span("read-only forge list"));

    if show_limit_hint {
        line.spans.push(help_action::footer_separator_span());
        line.spans.push(help_action::footer_muted_span(format!(
            "showing first {REQUESTED_REVIEW_DISPLAY_LIMIT}"
        )));
    }

    line
}

/// Builds the shared review-list block.
fn review_block() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title("Review Requests")
        .border_style(style::border_style())
}

/// Builds sectioned table rows for requested PRs or MRs, keeping personal
/// review requests visually separate from group-sourced requests.
fn review_rows(items: &[RequestedReview], title_column_width: usize) -> Vec<Row<'static>> {
    let mut rows = Vec::new();

    push_review_section(
        &mut rows,
        items,
        title_column_width,
        RequestedReviewAudience::Personal,
        "Requested from you",
    );
    push_review_section(
        &mut rows,
        items,
        title_column_width,
        RequestedReviewAudience::Group,
        "Requested from your groups",
    );

    rows
}

/// Appends one requested-review audience section when it contains rows.
fn push_review_section(
    rows: &mut Vec<Row<'static>>,
    items: &[RequestedReview],
    title_column_width: usize,
    audience: RequestedReviewAudience,
    label: &'static str,
) {
    let mut section_items = items
        .iter()
        .filter(|item| item.audience == audience)
        .peekable();
    if section_items.peek().is_none() {
        return;
    }

    rows.push(section_row(label));
    rows.extend(section_items.map(|item| review_row(item, title_column_width)));
}

/// Builds one visual section heading inside the requested-review table.
fn section_row(label: &'static str) -> Row<'static> {
    let cells = vec![
        Cell::from(label).style(Style::default().fg(style::palette::accent())),
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
    ];

    Row::new(cells).height(1)
}

/// Builds one table row for a requested PR or MR.
fn review_row(item: &RequestedReview, title_column_width: usize) -> Row<'static> {
    let review_label = format!(
        "{} {} {}",
        item.forge_kind.review_request_short_name(),
        item.display_id,
        item.title
    );
    let review_spans = vec![Span::raw(inline_text(&review_label))];
    let review_line = Line::from(truncate_spans_with_ellipsis(
        review_spans,
        title_column_width,
    ));
    let status = item
        .status_summary
        .clone()
        .unwrap_or_else(|| "Review requested".to_string());

    Row::new(vec![
        Cell::from(review_line),
        Cell::from(item.repository.clone()),
        Cell::from(status),
        Cell::from(updated_display(item.updated_at.as_deref())),
    ])
    .style(Style::default().fg(style::palette::text()))
    .height(1)
}

/// Returns a compact display date from a provider timestamp.
fn updated_display(updated_at: Option<&str>) -> String {
    updated_at
        .and_then(|value| value.split_once('T').map(|(date, _)| date.to_string()))
        .or_else(|| updated_at.map(str::to_string))
        .unwrap_or_else(|| "-".to_string())
}

/// Returns a bounded repository column width for the rendered items.
fn repository_column_width(items: &[RequestedReview]) -> Constraint {
    let width = items
        .iter()
        .map(|item| item.repository.chars().count())
        .max()
        .unwrap_or("Repository".len())
        .max("Repository".len());
    let width = u16::try_from(width).unwrap_or(u16::MAX).min(32);

    Constraint::Length(width)
}

/// Returns a bounded status column width for the rendered items.
fn status_column_width(items: &[RequestedReview]) -> Constraint {
    let width = items
        .iter()
        .map(|item| {
            item.status_summary
                .as_deref()
                .unwrap_or("Review requested")
                .chars()
                .count()
        })
        .max()
        .unwrap_or("Status".len())
        .max("Status".len());
    let width = u16::try_from(width).unwrap_or(u16::MAX).min(28);

    Constraint::Length(width)
}

#[cfg(test)]
mod tests {
    use ag_forge::ForgeKind;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::domain::theme::ColorTheme;

    #[test]
    fn test_render_loaded_reviews_groups_personal_and_group_rows() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let backend = TestBackend::new(100, 10);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let state = RequestedReviewState::Loaded {
            items: vec![
                requested_review(
                    RequestedReviewAudience::Personal,
                    ForgeKind::GitHub,
                    "#42",
                    "Add review tab",
                ),
                requested_review(
                    RequestedReviewAudience::Group,
                    ForgeKind::GitLab,
                    "!7",
                    "Polish merge request",
                ),
            ],
            project_id: 1,
        };

        // Act
        terminal
            .draw(|frame| {
                ReviewListPage::new(&state).render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Review Requests"));
        assert!(text.contains("Requested from you"));
        assert!(text.contains("PR #42 Add review tab"));
        assert!(text.contains("Requested from your groups"));
        assert!(text.contains("MR !7 Polish merge request"));

        let buffer = terminal.backend().buffer();
        let fallback_cell = &buffer.content()[0];
        let personal_group_cell =
            find_text_start_cell(buffer, "Requested from you").unwrap_or(fallback_cell);
        let member_group_cell =
            find_text_start_cell(buffer, "Requested from your groups").unwrap_or(fallback_cell);
        assert_eq!(personal_group_cell.fg, style::palette::accent());
        assert_eq!(member_group_cell.fg, style::palette::accent());
    }

    #[test]
    fn test_render_empty_reviews_shows_empty_state() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let backend = TestBackend::new(80, 8);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let state = RequestedReviewState::Loaded {
            items: Vec::new(),
            project_id: 1,
        };

        // Act
        terminal
            .draw(|frame| {
                ReviewListPage::new(&state).render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("No PRs or MRs currently request your review."));
    }

    #[test]
    fn test_render_reviews_at_limit_shows_truncation_hint() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let backend = TestBackend::new(110, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let state = RequestedReviewState::Loaded {
            items: (0..REQUESTED_REVIEW_DISPLAY_LIMIT)
                .map(|index| {
                    requested_review(
                        RequestedReviewAudience::Personal,
                        ForgeKind::GitHub,
                        &format!("#{index}"),
                        "Needs review",
                    )
                })
                .collect(),
            project_id: 1,
        };

        // Act
        terminal
            .draw(|frame| {
                ReviewListPage::new(&state).render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("showing first 100"));
        assert!(text.contains("read-only forge list"));
    }

    /// Builds one requested-review fixture for render tests.
    fn requested_review(
        audience: RequestedReviewAudience,
        forge_kind: ForgeKind,
        display_id: &str,
        title: &str,
    ) -> RequestedReview {
        RequestedReview {
            audience,
            display_id: display_id.to_string(),
            forge_kind,
            repository: "agentty-xyz/agentty".to_string(),
            status_summary: Some("Review requested".to_string()),
            title: title.to_string(),
            updated_at: Some("2026-04-27T21:30:00Z".to_string()),
            web_url: format!("https://example.com/{display_id}"),
        }
    }

    /// Extracts all rendered cell symbols from a test backend buffer.
    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<Vec<_>>()
            .join("")
    }

    /// Returns the first rendered cell that starts the requested text.
    fn find_text_start_cell<'a>(
        buffer: &'a ratatui::buffer::Buffer,
        needle: &str,
    ) -> Option<&'a ratatui::buffer::Cell> {
        let width = usize::from(buffer.area.width.max(1));
        let needle_symbols = needle
            .chars()
            .map(|character| character.to_string())
            .collect::<Vec<_>>();
        let content = buffer.content();

        for row_start in (0..content.len()).step_by(width) {
            let row_end = row_start + width.min(content.len().saturating_sub(row_start));
            let row = &content[row_start..row_end];

            for (index, window) in row.windows(needle_symbols.len()).enumerate() {
                let window_matches = window
                    .iter()
                    .zip(&needle_symbols)
                    .all(|(cell, symbol)| cell.symbol() == symbol);

                if window_matches {
                    return Some(&row[index]);
                }
            }
        }

        None
    }
}
