use std::slice;

use ag_forge::{
    ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewCommentThread,
};
use ag_tui_text::text_util;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::domain::session::Session;
use crate::presentation::app_mode::{ReviewCommentAction, ReviewCommentActionSelection};
use crate::presentation::review_comment as review_comment_selection;
use crate::ui::component::vertical_scrollbar::VerticalScrollbar;
use crate::ui::diff_util::DiffLine;
#[cfg(test)]
use crate::ui::diff_util::DiffLineKind;
use crate::ui::{Component, diff_util, markdown, review_comment_format, style};

const CODE_CONTEXT_RADIUS: usize = 3;

/// List and detail renderer embedded in the unified Diff workspace.
pub struct ReviewCommentPage<'a> {
    comment_actions: &'a [ReviewCommentActionSelection],
    comment_error: Option<&'a str>,
    comment_snapshot: Option<&'a ReviewCommentSnapshot>,
    diff: &'a str,
    is_loading_comments: bool,
    render_caches: ReviewCommentRenderCaches<'a>,
    scroll_offset: u16,
    selected_comment_index: usize,
    session: &'a Session,
}

/// Shared bounded caches used to derive review-comment detail rows.
#[derive(Clone, Copy)]
pub struct ReviewCommentRenderCaches<'a> {
    /// Parsed-diff cache shared with the main diff page.
    pub diff_layout: &'a crate::ui::page::diff::DiffLayoutCache,
    /// Styled Markdown cache shared with other text surfaces.
    pub markdown: &'a markdown::MarkdownRenderCache,
}

/// Borrowed inputs needed to construct one review-comment panel renderer.
#[derive(Clone, Copy)]
pub struct ReviewCommentPageInput<'a> {
    /// Actionable threads marked for batched address or deny handling.
    pub comment_actions: &'a [ReviewCommentActionSelection],
    /// User-facing failure returned by the forge comment load.
    pub comment_error: Option<&'a str>,
    /// Loaded general comments and inline review threads.
    pub comment_snapshot: Option<&'a ReviewCommentSnapshot>,
    /// Raw current session diff used to derive inline code context.
    pub diff: &'a str,
    /// Whether the forge comment request is still running.
    pub is_loading_comments: bool,
    /// Shared bounded caches used by paint and scroll metrics.
    pub render_caches: ReviewCommentRenderCaches<'a>,
    /// Vertical offset inside the selected comment detail panel.
    pub scroll_offset: u16,
    /// Selected general comment or inline thread index.
    pub selected_comment_index: usize,
    /// Session whose linked review request owns the comments.
    pub session: &'a Session,
}

impl<'a> ReviewCommentPage<'a> {
    /// Creates review-comment panels for one session frame.
    pub fn new(input: ReviewCommentPageInput<'a>) -> Self {
        let ReviewCommentPageInput {
            comment_actions,
            comment_error,
            comment_snapshot,
            diff,
            is_loading_comments,
            render_caches,
            scroll_offset,
            selected_comment_index,
            session,
        } = input;

        Self {
            comment_actions,
            comment_error,
            comment_snapshot,
            diff,
            is_loading_comments,
            render_caches,
            scroll_offset,
            selected_comment_index,
            session,
        }
    }

    /// Renders the left comment selector for loaded, loading, empty, and error
    /// states.
    pub(crate) fn render_comment_list(
        &self,
        frame: &mut Frame,
        area: Rect,
        rows: &[review_comment_selection::GroupedReviewCommentRow<'_>],
        is_focused: bool,
    ) {
        let item_count = review_comment_item_count(self.comment_snapshot);
        let title = format!(
            " Comments ({item_count}) · Marked {} ",
            self.comment_actions.len()
        );
        let (items, selection_rows) = if rows.is_empty() {
            (
                vec![ListItem::new(comment_list_fallback(
                    self.comment_error,
                    self.is_loading_comments,
                ))],
                Vec::new(),
            )
        } else {
            comment_list_items(rows, self.comment_actions)
        };
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(style::border_style()),
            )
            .highlight_style(
                Style::default()
                    .fg(style::palette::text())
                    .bg(style::palette::surface_selection())
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");
        let mut state = ListState::default();
        if is_focused && item_count > 0 {
            let selected_entry_index =
                normalized_selection(self.selected_comment_index, item_count);
            state.select(selection_rows.get(selected_entry_index).copied());
        }

        frame.render_stateful_widget(list, area, &mut state);
    }

    /// Renders metadata, conversation text, and attached code context for the
    /// selected comment entry.
    pub(crate) fn render_comment_detail(
        &self,
        frame: &mut Frame,
        area: Rect,
        rows: &[review_comment_selection::GroupedReviewCommentRow<'_>],
    ) {
        let content_width = usize::from(area.width.saturating_sub(2).max(1));
        let lines = comment_detail_lines(
            self.comment_snapshot.map(|_| rows),
            self.comment_error,
            self.is_loading_comments,
            self.diff,
            self.render_caches,
            self.selected_comment_index,
            content_width,
        );
        let viewport_height = area.height.saturating_sub(2);
        let line_count = lines.len();
        let max_scroll_offset = max_scroll_offset(line_count, viewport_height);
        let scroll_offset = self.scroll_offset.min(max_scroll_offset);
        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" Comment — {} ", self.session.display_title()))
                    .border_style(style::border_style()),
            )
            .scroll((scroll_offset, 0));

        frame.render_widget(paragraph, area);

        if max_scroll_offset > 0 {
            let scrollbar_area = diff_util::diff_scrollbar_area(area, viewport_height);
            VerticalScrollbar::new(scroll_offset, line_count).render(frame, scrollbar_area);
        }
    }
}

/// Returns the largest valid vertical offset for the selected comment detail.
pub(crate) fn review_comment_view_max_scroll_offset(
    comment_snapshot: Option<&ReviewCommentSnapshot>,
    comment_error: Option<&str>,
    is_loading_comments: bool,
    diff: &str,
    render_caches: ReviewCommentRenderCaches<'_>,
    selected_comment_index: usize,
    area: Rect,
) -> u16 {
    let detail_area = diff_util::diff_page_areas(area).diff_area;
    let viewport_height = detail_area.height.saturating_sub(2);
    let content_width = usize::from(detail_area.width.saturating_sub(2).max(1));
    let rows = comment_snapshot
        .map(review_comment_selection::grouped_review_comment_rows)
        .unwrap_or_default();
    let line_count = comment_detail_lines(
        comment_snapshot.map(|_| rows.as_slice()),
        comment_error,
        is_loading_comments,
        diff,
        render_caches,
        selected_comment_index,
        content_width,
    )
    .len();

    max_scroll_offset(line_count, viewport_height)
}

/// Returns the number of selectable general comments and inline threads.
pub(crate) fn review_comment_item_count(snapshot: Option<&ReviewCommentSnapshot>) -> usize {
    snapshot.map_or(0, |snapshot| {
        snapshot
            .pr_level_comments
            .len()
            .saturating_add(snapshot.threads.len())
    })
}

/// Returns whether the selected inline thread can be sent to the session
/// agent.
pub(crate) fn review_comment_selected_is_actionable(
    rows: &[review_comment_selection::GroupedReviewCommentRow<'_>],
    selected_comment_index: usize,
) -> bool {
    review_comment_selection::selected_entry(rows, selected_comment_index).is_some_and(
        |entry| matches!(entry, review_comment_selection::ReviewCommentEntry::Thread(thread) if thread.is_actionable()),
    )
}

/// Builds the batch-action marker aligned before one inline thread row.
fn review_comment_action_marker(
    thread: &ReviewCommentThread,
    selections: &[ReviewCommentActionSelection],
) -> Span<'static> {
    if !thread.is_actionable() {
        return Span::raw("    ");
    }

    match review_comment_selection::selected_action(selections, &thread.id) {
        Some(ReviewCommentAction::Address) => Span::styled(
            "[A] ",
            Style::default()
                .fg(style::palette::success())
                .add_modifier(Modifier::BOLD),
        ),
        Some(ReviewCommentAction::Deny) => Span::styled(
            "[D] ",
            Style::default()
                .fg(style::palette::danger())
                .add_modifier(Modifier::BOLD),
        ),
        None => Span::styled("[ ] ", Style::default().fg(style::palette::text_muted())),
    }
}

/// Builds group headings and selectable entry rows, returning each entry's
/// corresponding list-row index.
fn comment_list_items(
    rows: &[review_comment_selection::GroupedReviewCommentRow<'_>],
    selections: &[ReviewCommentActionSelection],
) -> (Vec<ListItem<'static>>, Vec<usize>) {
    let mut items = Vec::with_capacity(rows.len());
    let mut selection_rows = Vec::with_capacity(rows.len());

    for row in rows {
        match row {
            review_comment_selection::GroupedReviewCommentRow::Entry(entry) => {
                selection_rows.push(items.len());
                items.push(ListItem::new(comment_entry_label(*entry, selections)));
            }
            review_comment_selection::GroupedReviewCommentRow::GroupLabel(label) => {
                items.push(ListItem::new(Line::from(Span::styled(
                    *label,
                    Style::default()
                        .fg(style::palette::text_muted())
                        .add_modifier(Modifier::BOLD),
                ))));
            }
        }
    }

    (items, selection_rows)
}

/// Builds the compact label shown for one selectable comment entry.
fn comment_entry_label(
    entry: review_comment_selection::ReviewCommentEntry<'_>,
    selections: &[ReviewCommentActionSelection],
) -> Line<'static> {
    match entry {
        review_comment_selection::ReviewCommentEntry::General(comment) => Line::from(vec![
            Span::raw("    "),
            Span::styled("General", Style::default().fg(style::palette::accent())),
            Span::styled(
                format!(" · {}", comment.author),
                Style::default().fg(style::palette::text_muted()),
            ),
        ]),
        review_comment_selection::ReviewCommentEntry::Thread(thread) => {
            let anchor = review_comment_format::thread_anchor(thread);
            let author = thread
                .comments
                .first()
                .map_or("unknown", |comment| comment.author.as_str());

            Line::from(vec![
                review_comment_action_marker(thread, selections),
                Span::styled(anchor, Style::default().fg(style::palette::accent())),
                Span::styled(
                    format!(" · {author}"),
                    Style::default().fg(style::palette::text_muted()),
                ),
            ])
        }
    }
}

/// Builds all visible rows for one selected comment detail.
fn comment_detail_lines(
    rows: Option<&[review_comment_selection::GroupedReviewCommentRow<'_>]>,
    comment_error: Option<&str>,
    is_loading_comments: bool,
    diff: &str,
    render_caches: ReviewCommentRenderCaches<'_>,
    selected_comment_index: usize,
    width: usize,
) -> Vec<Line<'static>> {
    let Some(rows) = rows else {
        return vec![Line::from(comment_detail_fallback(
            comment_error,
            is_loading_comments,
        ))];
    };
    let item_count = review_comment_selection::selectable_entries(rows).count();
    let Some(entry) = review_comment_selection::selected_entry(
        rows,
        normalized_selection(selected_comment_index, item_count),
    ) else {
        return vec![Line::from("No review comments.")];
    };

    match entry {
        review_comment_selection::ReviewCommentEntry::General(comment) => {
            general_comment_detail_lines(comment, render_caches.markdown, width)
        }
        review_comment_selection::ReviewCommentEntry::Thread(thread) => {
            thread_comment_detail_lines(
                thread,
                diff,
                render_caches.diff_layout,
                render_caches.markdown,
                width,
            )
        }
    }
}

/// Builds metadata and body rows for one review-request-wide comment.
fn general_comment_detail_lines(
    comment: &ReviewComment,
    markdown_render_cache: &markdown::MarkdownRenderCache,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        field_line("Scope", "General discussion"),
        field_line("Author", &comment.author),
        Line::default(),
        section_line("Comment"),
    ];
    review_comment_format::append_comment_bodies(
        &mut lines,
        slice::from_ref(comment),
        markdown_render_cache,
        width,
    );
    lines.extend([
        Line::default(),
        section_line("Code context"),
        muted_line("This comment is not attached to a code line."),
    ]);

    lines
}

/// Builds metadata, current diff context, and thread conversation rows for an
/// inline review thread.
fn thread_comment_detail_lines(
    thread: &ReviewCommentThread,
    diff: &str,
    diff_layout_cache: &crate::ui::page::diff::DiffLayoutCache,
    markdown_render_cache: &markdown::MarkdownRenderCache,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        review_comment_format::thread_header_line(
            thread,
            Style::default()
                .fg(style::palette::accent())
                .add_modifier(Modifier::BOLD),
        ),
        Line::default(),
        section_line("Code context"),
    ];
    lines.extend(code_context_lines(thread, diff, diff_layout_cache, width));
    lines.extend([Line::default(), section_line("Conversation")]);
    review_comment_format::append_comment_bodies(
        &mut lines,
        &thread.comments,
        markdown_render_cache,
        width,
    );

    lines
}

/// Extracts nearby current-diff rows for the thread's file and anchor range.
fn code_context_lines(
    thread: &ReviewCommentThread,
    diff: &str,
    diff_layout_cache: &crate::ui::page::diff::DiffLayoutCache,
    width: usize,
) -> Vec<Line<'static>> {
    if thread.is_outdated == Some(true) {
        return vec![muted_line("Original code context unavailable.")];
    }

    if thread.anchor_side == ReviewCommentAnchorSide::File {
        return vec![muted_line(
            "This file-level comment is not attached to a code line.",
        )];
    }

    let parsed_content = diff_layout_cache.content(diff);
    let file_lines = parsed_content.file_lines(&thread.path);
    if file_lines.is_empty() {
        return vec![muted_line(
            "No current diff context is available for this file.",
        )];
    }

    let Some(anchor_line_range) = review_comment_format::thread_anchor_line_range(thread) else {
        return vec![muted_line("This comment has no attached line anchor.")];
    };
    let target_indexes = file_lines
        .iter()
        .position(|line| diff_line_matches_anchor(line, thread.anchor_side, anchor_line_range))
        .zip(file_lines.iter().rposition(|line| {
            diff_line_matches_anchor(line, thread.anchor_side, anchor_line_range)
        }));
    let Some((target_start_index, target_end_index)) = target_indexes else {
        return vec![muted_line(
            "The attached line or range is outside the current diff context.",
        )];
    };
    let start_index = target_start_index.saturating_sub(CODE_CONTEXT_RADIUS);
    let end_index = target_end_index
        .saturating_add(CODE_CONTEXT_RADIUS + 1)
        .min(file_lines.len());
    let gutter_width = diff_util::diff_line_gutter_width(&file_lines);

    file_lines[start_index..end_index]
        .iter()
        .map(|line| {
            let is_anchor = diff_line_matches_anchor(line, thread.anchor_side, anchor_line_range);

            code_context_line(line, is_anchor, gutter_width, width)
        })
        .collect()
}

/// Returns whether one diff row belongs to an inclusive thread anchor range.
fn diff_line_matches_anchor(
    line: &DiffLine<'_>,
    anchor_side: ReviewCommentAnchorSide,
    anchor_line_range: (u32, u32),
) -> bool {
    let line_number = match anchor_side {
        ReviewCommentAnchorSide::File => return false,
        ReviewCommentAnchorSide::New => line.new_line,
        ReviewCommentAnchorSide::Old => line.old_line,
    };
    let (start_line, end_line) = anchor_line_range;

    line_number.is_some_and(|line_number| (start_line..=end_line).contains(&line_number))
}

/// Formats one code-context row with old/new gutters and anchor emphasis.
fn code_context_line(
    line: &DiffLine<'_>,
    is_anchor: bool,
    gutter_width: usize,
    width: usize,
) -> Line<'static> {
    let (sign, content_style) = diff_util::body_diff_line_style(line.kind);
    let gutter_style = diff_util::body_diff_line_gutter_style();
    let (gutter_style, content_style) = if is_anchor {
        (
            gutter_style
                .bg(style::palette::surface_selection())
                .add_modifier(Modifier::BOLD),
            content_style
                .bg(style::palette::surface_selection())
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (gutter_style, content_style)
    };
    let gutter = diff_util::body_diff_line_gutter(line, gutter_width);
    let spans = vec![
        Span::styled(gutter, gutter_style),
        Span::styled(sign, content_style),
        Span::styled(line.content.to_string(), content_style),
    ];

    Line::from(text_util::truncate_spans_with_ellipsis(spans, width))
}

/// Returns the left-panel status label when no selectable entries exist.
fn comment_list_fallback(comment_error: Option<&str>, is_loading_comments: bool) -> &'static str {
    if comment_error.is_some() {
        return "Load failed";
    }
    if is_loading_comments {
        return "Loading...";
    }

    "No comments"
}

/// Returns the right-panel status text before comments are available.
fn comment_detail_fallback(comment_error: Option<&str>, is_loading_comments: bool) -> String {
    if let Some(comment_error) = comment_error {
        return comment_error.to_string();
    }
    if is_loading_comments {
        return "Loading review comments...".to_string();
    }

    "No review comments.".to_string()
}

/// Clamps a possibly stale selection to the loaded entry range.
fn normalized_selection(selected_index: usize, item_count: usize) -> usize {
    selected_index.min(item_count.saturating_sub(1))
}

/// Returns the largest scroll offset for a rendered detail line count.
fn max_scroll_offset(line_count: usize, viewport_height: u16) -> u16 {
    u16::try_from(line_count.saturating_sub(usize::from(viewport_height))).unwrap_or(u16::MAX)
}

/// Builds one bold metadata field.
fn field_line(label: &'static str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default()
                .fg(style::palette::text_muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            value.to_string(),
            Style::default().fg(style::palette::text()),
        ),
    ])
}

/// Builds one emphasized detail section label.
fn section_line(label: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        label,
        Style::default()
            .fg(style::palette::warning())
            .add_modifier(Modifier::BOLD),
    ))
}

/// Builds one muted informational row.
fn muted_line(text: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        text,
        Style::default().fg(style::palette::text_muted()),
    ))
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::domain::theme::ColorTheme;
    use crate::test_support::SessionFixtureBuilder;
    use crate::ui::component::vertical_scrollbar::SCROLLBAR_THUMB_SYMBOL;

    const SAMPLE_DIFF: &str = concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "index 1111111..2222222 100644\n",
        "--- a/src/main.rs\n",
        "+++ b/src/main.rs\n",
        "@@ -1,2 +1,3 @@\n",
        " fn main() {\n",
        "+    println!(\"review\");\n",
        " }\n",
    );

    fn inline_thread(line: u32) -> ReviewCommentThread {
        ReviewCommentThread {
            anchor_side: ReviewCommentAnchorSide::New,
            comments: vec![ReviewComment {
                author: "alice".to_string(),
                body: "Please explain this output.".to_string(),
            }],
            id: "thread-id".to_string(),
            is_outdated: Some(false),
            is_resolved: false,
            line: Some(line),
            path: "src/main.rs".to_string(),
            start_line: None,
        }
    }

    fn file_thread() -> ReviewCommentThread {
        ReviewCommentThread {
            anchor_side: ReviewCommentAnchorSide::File,
            comments: vec![ReviewComment {
                author: "bob".to_string(),
                body: "Please review the whole file.".to_string(),
            }],
            id: "thread-id".to_string(),
            is_outdated: Some(false),
            is_resolved: false,
            line: None,
            path: "src/main.rs".to_string(),
            start_line: None,
        }
    }

    fn comment_snapshot() -> ReviewCommentSnapshot {
        let mut thread = inline_thread(2);
        thread.comments[0].body = (0..12)
            .map(|line_number| format!("Inline comment line {line_number}"))
            .collect::<Vec<_>>()
            .join("\n");

        ReviewCommentSnapshot {
            pr_level_comments: vec![ReviewComment {
                author: "alice".to_string(),
                body: "General comment".to_string(),
            }],
            threads: vec![thread],
        }
    }

    fn render_review_comment_page(
        snapshot: Option<&ReviewCommentSnapshot>,
        comment_error: Option<&str>,
        is_loading_comments: bool,
        selected_comment_index: usize,
        scroll_offset: u16,
        terminal_size: (u16, u16),
    ) -> ratatui::buffer::Buffer {
        render_review_comment_page_with_actions(
            snapshot,
            &[],
            comment_error,
            is_loading_comments,
            selected_comment_index,
            scroll_offset,
            terminal_size,
        )
    }

    fn render_review_comment_page_with_actions(
        snapshot: Option<&ReviewCommentSnapshot>,
        comment_actions: &[ReviewCommentActionSelection],
        comment_error: Option<&str>,
        is_loading_comments: bool,
        selected_comment_index: usize,
        scroll_offset: u16,
        terminal_size: (u16, u16),
    ) -> ratatui::buffer::Buffer {
        let session = SessionFixtureBuilder::new()
            .title(Some("Review comments session".to_string()))
            .build();
        let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let backend = TestBackend::new(terminal_size.0, terminal_size.1);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        terminal
            .draw(|frame| {
                let page = ReviewCommentPage::new(ReviewCommentPageInput {
                    comment_actions,
                    comment_error,
                    comment_snapshot: snapshot,
                    diff: SAMPLE_DIFF,
                    is_loading_comments,
                    render_caches: ReviewCommentRenderCaches {
                        diff_layout: &diff_layout_cache,
                        markdown: &markdown_render_cache,
                    },
                    scroll_offset,
                    selected_comment_index,
                    session: &session,
                });
                let areas = diff_util::diff_page_areas(frame.area());
                let rows = snapshot
                    .map(review_comment_selection::grouped_review_comment_rows)
                    .unwrap_or_default();
                page.render_comment_list(frame, areas.file_list_area, &rows, true);
                page.render_comment_detail(frame, areas.diff_area, &rows);
            })
            .expect("failed to draw review-comment page");

        terminal.backend().buffer().clone()
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn test_render_shows_comment_selector_and_general_detail() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let snapshot = comment_snapshot();

        // Act
        let buffer = render_review_comment_page(Some(&snapshot), None, false, 1, 0, (140, 24));
        let text = buffer_text(&buffer);

        // Assert
        assert!(text.contains("Comments (2)"));
        assert!(text.contains("Unresolved"));
        assert!(text.contains("Standalone"));
        assert!(text.contains("General · alice"));
        assert!(text.contains("src/main.rs:2"));
        assert!(text.contains("Comment — Review comments session"));
        assert!(text.contains("Scope: General discussion"));
        assert!(text.contains("General comment"));
        assert!(text.contains("This comment is not attached to a code line."));
        assert!(
            buffer
                .content()
                .iter()
                .any(|cell| cell.bg == style::palette::surface_selection())
        );
    }

    #[test]
    fn test_render_shows_selected_inline_context_and_scrollbar() {
        // Arrange
        let snapshot = comment_snapshot();

        // Act
        let buffer = render_review_comment_page(Some(&snapshot), None, false, 0, 0, (100, 14));
        let text = buffer_text(&buffer);

        // Assert
        assert!(text.contains("src/main.rs:2"));
        assert!(text.contains("Code context"));
        assert!(text.contains("println!(\"review\")"));
        assert!(text.contains(SCROLLBAR_THUMB_SYMBOL));
    }

    #[test]
    fn test_render_shows_batched_address_and_deny_markers() {
        // Arrange
        let mut address_thread = inline_thread(2);
        address_thread.id = "address".to_string();
        let mut deny_thread = inline_thread(3);
        deny_thread.id = "deny".to_string();
        let mut resolved_thread = inline_thread(4);
        resolved_thread.id = "resolved".to_string();
        resolved_thread.is_resolved = true;
        let snapshot = ReviewCommentSnapshot {
            pr_level_comments: Vec::new(),
            threads: vec![address_thread, deny_thread, resolved_thread],
        };
        let comment_actions = vec![
            ReviewCommentActionSelection {
                action: ReviewCommentAction::Address,
                thread_id: "address".to_string(),
            },
            ReviewCommentActionSelection {
                action: ReviewCommentAction::Deny,
                thread_id: "deny".to_string(),
            },
        ];

        // Act
        let buffer = render_review_comment_page_with_actions(
            Some(&snapshot),
            &comment_actions,
            None,
            false,
            0,
            0,
            (140, 24),
        );
        let text = buffer_text(&buffer);

        // Assert
        assert!(text.contains("[A] src/main.rs:2"));
        assert!(text.contains("[D] src/main.rs:3"));
        assert!(text.contains("src/main.rs:4"));
    }

    #[test]
    fn test_render_shows_loading_error_and_empty_fallbacks() {
        // Arrange
        let empty_snapshot = ReviewCommentSnapshot {
            pr_level_comments: Vec::new(),
            threads: Vec::new(),
        };

        // Act
        let loading = buffer_text(&render_review_comment_page(
            None,
            None,
            true,
            0,
            0,
            (80, 12),
        ));
        let error = buffer_text(&render_review_comment_page(
            None,
            Some("Forge unavailable"),
            false,
            0,
            0,
            (80, 12),
        ));
        let empty = buffer_text(&render_review_comment_page(
            Some(&empty_snapshot),
            None,
            false,
            0,
            0,
            (80, 12),
        ));
        let missing = buffer_text(&render_review_comment_page(
            None,
            None,
            false,
            0,
            0,
            (80, 12),
        ));

        // Assert
        assert!(loading.contains("Loading..."));
        assert!(loading.contains("Loading review comments..."));
        assert!(error.contains("Load failed"));
        assert!(error.contains("Forge unavailable"));
        assert!(empty.contains("No comments"));
        assert!(empty.contains("No review comments."));
        assert!(missing.contains("No review comments."));
    }

    #[test]
    fn test_review_comment_view_max_scroll_offset_reflects_detail_overflow() {
        // Arrange
        let snapshot = comment_snapshot();
        let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();

        // Act
        let scroll_offset = review_comment_view_max_scroll_offset(
            Some(&snapshot),
            None,
            false,
            SAMPLE_DIFF,
            ReviewCommentRenderCaches {
                diff_layout: &diff_layout_cache,
                markdown: &markdown_render_cache,
            },
            0,
            Rect::new(0, 0, 80, 10),
        );

        // Assert
        assert!(scroll_offset > 0);
    }

    #[test]
    fn test_code_context_lines_include_and_highlight_attached_new_line() {
        // Arrange
        let thread = inline_thread(2);
        let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();

        // Act
        let lines = code_context_lines(&thread, SAMPLE_DIFF, &diff_layout_cache, 80);

        // Assert
        let attached_line = lines
            .iter()
            .find(|line| line.to_string().contains("println!"))
            .expect("attached line should be visible");
        assert!(attached_line.to_string().contains("+    println!"));
        assert!(
            attached_line
                .spans
                .iter()
                .all(|span| span.style.bg == Some(style::palette::surface_selection()))
        );
    }

    #[test]
    fn test_code_context_lines_repeat_inline_derivation_with_one_shared_cache() {
        // Arrange
        let thread = inline_thread(2);
        let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();
        let diff = concat!(
            "diff --git a/src/unrelated.rs b/src/unrelated.rs\n",
            "@@ -1 +1 @@\n",
            "-unrelated old\n",
            "+unrelated new\n",
            "diff --git a/src/main.rs b/src/main.rs\n",
            "@@ -1,3 +1,4 @@\n",
            " fn main() {\n",
            "+    println!(\"hello\");\n",
            " }\n",
        );

        // Act
        let first_lines = code_context_lines(&thread, diff, &diff_layout_cache, 80);
        let repeated_lines = code_context_lines(&thread, diff, &diff_layout_cache, 80);

        // Assert
        assert_eq!(first_lines, repeated_lines);
        assert!(
            repeated_lines
                .iter()
                .all(|line| !line.to_string().contains("unrelated"))
        );
        assert!(
            repeated_lines
                .iter()
                .any(|line| line.to_string().contains("println!"))
        );
    }

    #[test]
    fn test_code_context_lines_highlight_every_line_in_multiline_anchor_range() {
        // Arrange
        let mut thread = inline_thread(2);
        thread.start_line = Some(1);
        let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();

        // Act
        let lines = code_context_lines(&thread, SAMPLE_DIFF, &diff_layout_cache, 80);

        // Assert
        let start_line = lines
            .iter()
            .find(|line| line.to_string().contains("fn main"))
            .expect("attached range start should be visible");
        let end_line = lines
            .iter()
            .find(|line| line.to_string().contains("println!"))
            .expect("attached range end should be visible");
        let trailing_line = lines
            .iter()
            .find(|line| line.to_string().contains('}'))
            .expect("surrounding context should be visible");
        assert!(
            start_line
                .spans
                .iter()
                .all(|span| span.style.bg == Some(style::palette::surface_selection()))
        );
        assert!(
            end_line
                .spans
                .iter()
                .all(|span| span.style.bg == Some(style::palette::surface_selection()))
        );
        assert!(
            trailing_line
                .spans
                .iter()
                .all(|span| span.style.bg.is_none())
        );
    }

    #[test]
    fn test_code_context_line_uses_diff_page_gutter_and_change_styles() {
        // Arrange
        let addition = DiffLine {
            content: "added",
            kind: DiffLineKind::Addition,
            new_line: Some(2),
            old_line: None,
        };
        let deletion = DiffLine {
            content: "removed",
            kind: DiffLineKind::Deletion,
            new_line: None,
            old_line: Some(2),
        };

        // Act
        let addition_line = code_context_line(&addition, false, 1, 80);
        let deletion_line = code_context_line(&deletion, false, 1, 80);

        // Assert
        assert_eq!(
            addition_line.spans[0].style.fg,
            Some(style::palette::text_subtle())
        );
        assert_eq!(
            addition_line.spans[1].style.bg,
            Some(style::palette::surface_success())
        );
        assert_eq!(
            deletion_line.spans[0].style.fg,
            Some(style::palette::text_subtle())
        );
        assert_eq!(
            deletion_line.spans[1].style.bg,
            Some(style::palette::surface_danger())
        );
    }

    #[test]
    fn test_code_context_lines_explain_file_level_anchor_without_synthetic_code() {
        // Arrange
        let thread = file_thread();
        let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();

        // Act
        let lines = code_context_lines(&thread, SAMPLE_DIFF, &diff_layout_cache, 80);

        // Assert
        assert_eq!(
            lines.iter().map(Line::to_string).collect::<Vec<_>>(),
            vec!["This file-level comment is not attached to a code line."]
        );
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.style.bg.is_none())
        );
    }

    #[test]
    fn test_code_context_lines_explain_outdated_anchor_without_current_diff_context() {
        // Arrange
        let mut thread = inline_thread(2);
        thread.is_outdated = Some(true);
        let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();

        // Act
        let lines = code_context_lines(&thread, SAMPLE_DIFF, &diff_layout_cache, 80);

        // Assert
        assert_eq!(
            lines.iter().map(Line::to_string).collect::<Vec<_>>(),
            vec!["Original code context unavailable."]
        );
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.style.bg.is_none())
        );
    }

    #[test]
    fn test_code_context_lines_cover_old_side_and_missing_anchor_fallbacks() {
        // Arrange
        let mut old_thread = inline_thread(1);
        old_thread.anchor_side = ReviewCommentAnchorSide::Old;
        let mut missing_file_thread = inline_thread(2);
        missing_file_thread.path = "src/missing.rs".to_string();
        let outside_thread = inline_thread(99);
        let mut missing_anchor_thread = inline_thread(2);
        missing_anchor_thread.line = None;
        let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();

        // Act
        let old_lines = code_context_lines(&old_thread, SAMPLE_DIFF, &diff_layout_cache, 80);
        let missing_file_lines =
            code_context_lines(&missing_file_thread, SAMPLE_DIFF, &diff_layout_cache, 80);
        let outside_lines =
            code_context_lines(&outside_thread, SAMPLE_DIFF, &diff_layout_cache, 80);
        let missing_anchor_lines =
            code_context_lines(&missing_anchor_thread, SAMPLE_DIFF, &diff_layout_cache, 80);
        let file_side_matches = diff_line_matches_anchor(
            &DiffLine {
                content: "file",
                kind: DiffLineKind::Context,
                new_line: Some(1),
                old_line: Some(1),
            },
            ReviewCommentAnchorSide::File,
            (1, 1),
        );

        // Assert
        let old_anchor = old_lines
            .iter()
            .find(|line| line.to_string().contains("fn main"))
            .expect("old-side anchor should be visible");
        assert!(
            old_anchor
                .spans
                .iter()
                .all(|span| span.style.bg == Some(style::palette::surface_selection()))
        );
        assert_eq!(
            missing_file_lines[0].to_string(),
            "No current diff context is available for this file."
        );
        assert_eq!(
            outside_lines[0].to_string(),
            "The attached line or range is outside the current diff context."
        );
        assert_eq!(
            missing_anchor_lines[0].to_string(),
            "This comment has no attached line anchor."
        );
        assert!(!file_side_matches);
    }

    #[test]
    fn test_comment_detail_lines_include_thread_metadata_body_and_code() {
        // Arrange
        let snapshot = ReviewCommentSnapshot {
            pr_level_comments: Vec::new(),
            threads: vec![inline_thread(2)],
        };
        let rows = review_comment_selection::grouped_review_comment_rows(&snapshot);
        let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();

        // Act
        let lines = comment_detail_lines(
            Some(&rows),
            None,
            false,
            SAMPLE_DIFF,
            ReviewCommentRenderCaches {
                diff_layout: &diff_layout_cache,
                markdown: &markdown_render_cache,
            },
            0,
            80,
        );
        let text = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let code_context_index = lines
            .iter()
            .position(|line| line.to_string() == "Code context")
            .expect("code context section should be visible");
        let conversation_index = lines
            .iter()
            .position(|line| line.to_string() == "Conversation")
            .expect("conversation section should be visible");

        // Assert
        assert!(text.contains("src/main.rs:2"));
        assert!(text.contains("Please explain this output."));
        assert!(text.contains("Code context"));
        assert!(text.contains("println!(\"review\")"));
        assert!(code_context_index < conversation_index);
    }

    #[test]
    fn test_review_comment_item_count_includes_general_comments_and_threads() {
        // Arrange
        let snapshot = ReviewCommentSnapshot {
            pr_level_comments: vec![ReviewComment {
                author: "bob".to_string(),
                body: "General note".to_string(),
            }],
            threads: vec![inline_thread(2), inline_thread(3)],
        };

        // Act
        let count = review_comment_item_count(Some(&snapshot));

        // Assert
        assert_eq!(count, 3);
    }

    #[test]
    fn test_comment_list_items_map_grouped_rows_to_selectable_indexes() {
        // Arrange
        let mut resolved = inline_thread(3);
        resolved.id = "resolved".to_string();
        resolved.is_resolved = true;
        let mut unresolved = inline_thread(2);
        unresolved.id = "unresolved".to_string();
        let snapshot = ReviewCommentSnapshot {
            pr_level_comments: vec![ReviewComment {
                author: "bob".to_string(),
                body: "Standalone note".to_string(),
            }],
            threads: vec![resolved, unresolved],
        };

        // Act
        let rows = review_comment_selection::grouped_review_comment_rows(&snapshot);
        let (items, selection_rows) = comment_list_items(&rows, &[]);

        // Assert
        assert_eq!(items.len(), 6);
        assert_eq!(selection_rows, vec![1, 3, 5]);
    }

    #[test]
    fn test_review_comment_actionability_includes_outdated_unresolved_rows() {
        // Arrange
        let mut current = inline_thread(2);
        current.id = "current".to_string();
        let mut resolved = inline_thread(3);
        resolved.id = "resolved".to_string();
        resolved.is_resolved = true;
        let mut outdated = inline_thread(4);
        outdated.id = "outdated".to_string();
        outdated.is_outdated = Some(true);
        let snapshot = ReviewCommentSnapshot {
            pr_level_comments: vec![ReviewComment {
                author: "bob".to_string(),
                body: "General note".to_string(),
            }],
            threads: vec![current, resolved, outdated],
        };
        let rows = review_comment_selection::grouped_review_comment_rows(&snapshot);

        // Act
        let current_is_actionable = review_comment_selected_is_actionable(&rows, 0);
        let outdated_is_actionable = review_comment_selected_is_actionable(&rows, 1);
        let resolved_is_actionable = review_comment_selected_is_actionable(&rows, 2);
        let general_is_actionable = review_comment_selected_is_actionable(&rows, 3);
        let missing_is_actionable = review_comment_selected_is_actionable(&rows, 99);
        let current_thread_id = review_comment_selection::selected_thread_id(&snapshot, 0);
        let general_thread_id = review_comment_selection::selected_thread_id(&snapshot, 3);

        // Assert
        assert!(!general_is_actionable);
        assert!(current_is_actionable);
        assert!(!resolved_is_actionable);
        assert!(outdated_is_actionable);
        assert!(!missing_is_actionable);
        assert_eq!(current_thread_id, Some("current"));
        assert_eq!(general_thread_id, None);
        assert!(!review_comment_selected_is_actionable(&[], 0));
    }

    #[test]
    fn test_review_comment_actionability_is_false_without_general_or_current_threads() {
        // Arrange
        let mut resolved = inline_thread(2);
        resolved.is_resolved = true;
        let snapshot = ReviewCommentSnapshot {
            pr_level_comments: vec![ReviewComment {
                author: "bob".to_string(),
                body: "Standalone note".to_string(),
            }],
            threads: vec![resolved],
        };
        let rows = review_comment_selection::grouped_review_comment_rows(&snapshot);

        // Act, Assert
        assert!(!review_comment_selected_is_actionable(&rows, 0));
        assert!(!review_comment_selected_is_actionable(&rows, 1));
    }
}
