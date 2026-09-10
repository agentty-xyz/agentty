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
use crate::presentation::app_mode::ReviewCommentSelection;
use crate::presentation::review_comment as review_comment_selection;
use crate::ui::component::vertical_scrollbar::VerticalScrollbar;
use crate::ui::diff_util::DiffLine;
use crate::ui::{Component, diff_util, markdown, review_comment_format, style};

const CODE_CONTEXT_RADIUS: usize = 3;

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
    /// Actionable threads selected for batched agent evaluation.
    pub selected_comments: &'a [ReviewCommentSelection],
    /// Session whose linked review request owns the comments.
    pub session: &'a Session,
}

/// List and detail renderer embedded in the unified Diff workspace.
pub struct ReviewCommentPage<'a> {
    comment_error: Option<&'a str>,
    comment_snapshot: Option<&'a ReviewCommentSnapshot>,
    diff: &'a str,
    is_loading_comments: bool,
    render_caches: ReviewCommentRenderCaches<'a>,
    scroll_offset: u16,
    selected_comment_index: usize,
    selected_comments: &'a [ReviewCommentSelection],
    session: &'a Session,
}

impl<'a> ReviewCommentPage<'a> {
    /// Creates review-comment panels for one session frame.
    pub fn new(input: ReviewCommentPageInput<'a>) -> Self {
        let ReviewCommentPageInput {
            selected_comments,
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
            comment_error,
            comment_snapshot,
            diff,
            is_loading_comments,
            render_caches,
            scroll_offset,
            selected_comment_index,
            selected_comments,
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
            " Comments ({item_count}) · Selected {} ",
            self.selected_comments.len()
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
            comment_list_items(rows, self.selected_comments)
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

/// Builds the selection marker aligned before one inline thread row.
fn review_comment_selection_marker(
    thread: &ReviewCommentThread,
    selections: &[ReviewCommentSelection],
) -> Span<'static> {
    if !thread.is_actionable() {
        return Span::raw("    ");
    }

    if review_comment_selection::is_selected(selections, &thread.id) {
        Span::styled(
            "[x] ",
            Style::default()
                .fg(style::palette::success())
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled("[ ] ", Style::default().fg(style::palette::text_muted()))
    }
}

/// Builds group headings and selectable entry rows, returning each entry's
/// corresponding list-row index.
fn comment_list_items(
    rows: &[review_comment_selection::GroupedReviewCommentRow<'_>],
    selections: &[ReviewCommentSelection],
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
    selections: &[ReviewCommentSelection],
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
                review_comment_selection_marker(thread, selections),
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
#[path = "review_comment_test.rs"]
mod tests;
