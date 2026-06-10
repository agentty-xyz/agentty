use ag_forge::{RequestedReview, RequestedReviewAudience};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use crate::app::RequestedReviewState;
use crate::ui::input_layout::first_table_column_width;
use crate::ui::state::help_action;
use crate::ui::text_util::{inline_text, truncate_spans_with_ellipsis};
use crate::ui::{Page, layout, style};

/// Horizontal spacing between requested-review table columns.
const TABLE_COLUMN_SPACING: u16 = 2;
/// Empty highlight symbol keeps selected review rows aligned with unselected
/// rows.
const ROW_HIGHLIGHT_SYMBOL: &str = "";
/// Requested-review provider row cap surfaced by the footer when reached.
const REQUESTED_REVIEW_DISPLAY_LIMIT: usize = 100;
/// Page renderer for PRs and MRs requesting the current user's review.
pub struct ReviewListPage<'a> {
    /// Current requested-review cache snapshot to render.
    requested_reviews: &'a RequestedReviewState,
    /// Selected requested-review item index, excluding section heading rows.
    selected_review_index: Option<usize>,
    /// Persistent table selection and viewport state for review rows.
    table_state: &'a mut TableState,
}

impl<'a> ReviewListPage<'a> {
    /// Creates a requested-review list renderer from the current app cache.
    pub fn new(
        requested_reviews: &'a RequestedReviewState,
        selected_review_index: Option<usize>,
        table_state: &'a mut TableState,
    ) -> Self {
        Self {
            requested_reviews,
            selected_review_index,
            table_state,
        }
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
                render_review_table(
                    f,
                    areas.main_area,
                    items,
                    self.selected_review_index,
                    self.table_state,
                );
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
fn render_review_table(
    f: &mut Frame,
    area: Rect,
    items: &[RequestedReview],
    selected_review_index: Option<usize>,
    table_state: &mut TableState,
) {
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
    let table_rows = review_table_rows(items);
    let rows = table_rows
        .iter()
        .map(|row| row.render(title_column_width))
        .collect::<Vec<_>>();
    let selected_row = selected_render_row(&table_rows, selected_review_index);
    prepare_review_table_state(table_state, selected_row);
    let table = Table::new(rows, constraints)
        .column_spacing(TABLE_COLUMN_SPACING)
        .header(header)
        .block(block)
        .row_highlight_style(Style::default().bg(style::palette::surface()))
        .highlight_symbol(ROW_HIGHLIGHT_SYMBOL);

    f.render_stateful_widget(table, area, table_state);
}

/// Updates the selected rendered row while preserving Ratatui's table
/// viewport offset across frames.
fn prepare_review_table_state(table_state: &mut TableState, selected_row: Option<usize>) {
    table_state.select(selected_row);
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

/// Intermediate row model shared by table rendering and selected-row mapping.
enum ReviewTableRow<'a> {
    /// Non-selectable audience section heading.
    Section {
        /// Visible section label.
        label: &'static str,
    },
    /// Selectable requested-review row.
    Review {
        /// Requested-review item rendered on this table row.
        item: &'a RequestedReview,
        /// Original item index in the loaded requested-review list.
        item_index: usize,
    },
}

impl ReviewTableRow<'_> {
    /// Converts this row model into a Ratatui table row.
    fn render(&self, title_column_width: usize) -> Row<'static> {
        match self {
            Self::Section { label } => section_row(label),
            Self::Review { item, .. } => review_row(item, title_column_width),
        }
    }
}

/// Builds sectioned table row models for requested PRs or MRs, keeping
/// personal review requests visually separate from group-sourced requests.
fn review_table_rows(items: &[RequestedReview]) -> Vec<ReviewTableRow<'_>> {
    let mut rows = Vec::new();

    push_review_section(
        &mut rows,
        items,
        RequestedReviewAudience::Personal,
        "Requested from you",
    );
    push_review_section(
        &mut rows,
        items,
        RequestedReviewAudience::Group,
        "Requested from your groups",
    );

    rows
}

/// Appends one requested-review audience section when it contains rows.
fn push_review_section<'a>(
    rows: &mut Vec<ReviewTableRow<'a>>,
    items: &'a [RequestedReview],
    audience: RequestedReviewAudience,
    label: &'static str,
) {
    let mut section_items = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.audience == audience)
        .peekable();
    if section_items.peek().is_none() {
        return;
    }

    rows.push(ReviewTableRow::Section { label });
    rows.extend(
        section_items.map(|(item_index, item)| ReviewTableRow::Review { item, item_index }),
    );
}

/// Maps a requested-review item selection to the rendered table row,
/// accounting for audience section headers.
fn selected_render_row(
    rows: &[ReviewTableRow<'_>],
    selected_review_index: Option<usize>,
) -> Option<usize> {
    let selected_review_index = selected_review_index?;

    rows.iter().position(|row| {
        matches!(
            row,
            ReviewTableRow::Review { item_index, .. } if *item_index == selected_review_index
        )
    })
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
        let mut table_state = TableState::default();
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
                ReviewListPage::new(&state, Some(0), &mut table_state).render(frame, frame.area());
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
        let mut table_state = TableState::default();
        let state = RequestedReviewState::Loaded {
            items: Vec::new(),
            project_id: 1,
        };

        // Act
        terminal
            .draw(|frame| {
                ReviewListPage::new(&state, None, &mut table_state).render(frame, frame.area());
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
        let backend = TestBackend::new(140, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
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
                ReviewListPage::new(&state, Some(0), &mut table_state).render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("showing first 100"));
        assert!(text.contains("read-only forge list"));
    }

    #[test]
    fn test_render_loaded_reviews_highlights_selected_request() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let backend = TestBackend::new(100, 10);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
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
                ReviewListPage::new(&state, Some(1), &mut table_state).render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let selected_cell = find_text_start_cell(buffer, "MR !7 Polish merge request")
            .expect("selected review row should render");
        let unselected_cell = find_text_start_cell(buffer, "PR #42 Add review tab")
            .expect("unselected review row should render");
        assert_eq!(selected_cell.bg, style::palette::surface());
        assert_ne!(unselected_cell.bg, style::palette::surface());
    }

    #[test]
    fn test_selected_render_row_uses_review_table_rows() {
        // Arrange
        let items = vec![
            requested_review(
                RequestedReviewAudience::Personal,
                ForgeKind::GitHub,
                "#42",
                "Personal review",
            ),
            requested_review(
                RequestedReviewAudience::Group,
                ForgeKind::GitLab,
                "!7",
                "First group review",
            ),
            requested_review(
                RequestedReviewAudience::Group,
                ForgeKind::GitHub,
                "#8",
                "Second group review",
            ),
        ];
        let rows = review_table_rows(&items);

        // Act
        let personal_row = selected_render_row(&rows, Some(0));
        let first_group_row = selected_render_row(&rows, Some(1));
        let second_group_row = selected_render_row(&rows, Some(2));
        let out_of_range_row = selected_render_row(&rows, Some(3));

        // Assert
        assert_eq!(personal_row, Some(1));
        assert_eq!(first_group_row, Some(3));
        assert_eq!(second_group_row, Some(4));
        assert_eq!(out_of_range_row, None);
    }

    #[test]
    fn test_prepare_review_table_state_preserves_viewport_offset() {
        // Arrange
        let mut table_state = TableState::default();
        *table_state.offset_mut() = 12;
        table_state.select(Some(18));

        // Act
        prepare_review_table_state(&mut table_state, Some(17));

        // Assert
        assert_eq!(table_state.offset(), 12);
        assert_eq!(table_state.selected(), Some(17));
    }

    #[test]
    fn test_render_loaded_reviews_reuses_table_viewport_offset() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let backend = TestBackend::new(100, 8);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
        let state = RequestedReviewState::Loaded {
            items: (0..30)
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
                ReviewListPage::new(&state, Some(20), &mut table_state).render(frame, frame.area());
            })
            .expect("failed to draw selected row");
        let offset_after_first_render = table_state.offset();

        terminal
            .draw(|frame| {
                ReviewListPage::new(&state, Some(19), &mut table_state).render(frame, frame.area());
            })
            .expect("failed to draw previous selected row");

        // Assert
        assert!(offset_after_first_render > 0);
        assert_eq!(table_state.offset(), offset_after_first_render);
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
            body: Some("Review body".to_string()),
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
