use std::sync::Arc;

use ag_forge::{ReviewComment, ReviewCommentSnapshot, ReviewCommentThread, ReviewRequestState};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::domain::session::{ReviewRequest, Session};
use crate::ui::component::file_explorer::FileExplorer;
use crate::ui::state::app_mode::DiffRightPanel;
use crate::ui::state::help_action;
use crate::ui::text_util::wrap_lines_to_rows;
use crate::ui::util::{DiffLine, DiffLineKind, inline_text, parse_diff_lines, selected_diff_lines};
use crate::ui::{Component, Page, diff_util, markdown, style};

const SCROLL_X_OFFSET: u16 = 0;
const SCROLLBAR_TRACK_SYMBOL: &str = "│";
const SCROLLBAR_THUMB_SYMBOL: &str = "█";
const WRAPPED_CHUNK_START_INDEX: usize = 0;

/// Renders the current session's git diff in a scrollable page.
///
/// The right-hand panel shows either the raw git diff or cached review
/// comments, selected by [`DiffPage::right_panel`]. The left-hand file explorer
/// is unchanged across panels; the `c` key toggles the right-hand view.
pub struct DiffPage<'a> {
    pub diff: String,
    pub file_explorer_selected_index: usize,
    pub markdown_render_cache: &'a markdown::MarkdownRenderCache,
    pub review_comment_snapshot: Option<&'a ReviewCommentSnapshot>,
    pub right_panel: DiffRightPanel,
    pub scroll_offset: u16,
    pub session: &'a Session,
}

impl<'a> DiffPage<'a> {
    /// Creates a diff page for the given session, scroll position, and right
    /// panel selection.
    pub fn new(
        session: &'a Session,
        diff: String,
        scroll_offset: u16,
        file_explorer_selected_index: usize,
        right_panel: DiffRightPanel,
        markdown_render_cache: &'a markdown::MarkdownRenderCache,
        review_comment_snapshot: Option<&'a ReviewCommentSnapshot>,
    ) -> Self {
        Self {
            diff,
            file_explorer_selected_index,
            markdown_render_cache,
            review_comment_snapshot,
            right_panel,
            scroll_offset,
            session,
        }
    }

    /// Renders the right-side diff panel with line-number gutters and
    /// change totals prefixed in the title.
    fn render_diff_content(
        &self,
        f: &mut Frame,
        area: Rect,
        parsed: &[DiffLine],
        total_added_lines: u64,
        total_removed_lines: u64,
    ) {
        let title = Line::from(vec![
            Span::styled(" (", Style::default().fg(style::palette::warning())),
            Span::styled(
                format!("+{total_added_lines}"),
                Style::default().fg(style::palette::success()),
            ),
            Span::styled(" ", Style::default().fg(style::palette::warning())),
            Span::styled(
                format!("-{total_removed_lines}"),
                Style::default().fg(style::palette::danger()),
            ),
            Span::styled(
                format!(") Diff — {} ", inline_text(self.session.display_title())),
                Style::default().fg(style::palette::warning()),
            ),
        ]);

        let mut layout = diff_util::diff_render_layout(parsed, area, false);
        let mut lines = Self::build_diff_lines(parsed, layout);
        let mut show_scrollbar =
            diff_util::diff_has_scrollable_overflow(lines.len(), layout.viewport_height);

        if show_scrollbar {
            layout = diff_util::diff_render_layout(parsed, area, true);
            lines = Self::build_diff_lines(parsed, layout);
            show_scrollbar =
                diff_util::diff_has_scrollable_overflow(lines.len(), layout.viewport_height);
        }
        let total_lines = lines.len();

        let scroll_offset = diff_util::clamp_diff_scroll_offset(
            self.scroll_offset,
            total_lines,
            layout.viewport_height,
        );

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(style::border_style()),
            )
            .scroll((scroll_offset, SCROLL_X_OFFSET));

        f.render_widget(paragraph, area);

        if show_scrollbar {
            Self::render_diff_scrollbar(
                f,
                area,
                layout.viewport_height,
                scroll_offset,
                total_lines,
            );
        }
    }

    /// Returns the style used for added diff lines.
    fn addition_line_style() -> Style {
        Style::default()
            .fg(style::palette::success())
            .bg(style::palette::surface_success())
    }

    /// Returns the style used for removed diff lines.
    fn deletion_line_style() -> Style {
        Style::default()
            .fg(style::palette::danger())
            .bg(style::palette::surface_danger())
    }

    /// Builds wrapped diff lines for the diff panel, optionally reserving one
    /// column for the scrollbar thumb.
    fn build_diff_lines<'line>(
        parsed: &[DiffLine<'line>],
        layout: diff_util::DiffRenderLayout,
    ) -> Vec<Line<'line>> {
        let gutter_style = Style::default().fg(style::palette::text_subtle());
        let mut lines: Vec<Line<'line>> = Vec::with_capacity(parsed.len());

        for diff_line in parsed {
            let (sign, content_style) = match diff_line.kind {
                DiffLineKind::FileHeader => {
                    if diff_line.content.starts_with("diff ") && !lines.is_empty() {
                        lines.push(Line::from(""));
                    }
                    lines.push(Line::from(Span::styled(
                        diff_line.content,
                        Style::default().fg(style::palette::warning()),
                    )));

                    continue;
                }
                DiffLineKind::HunkHeader => {
                    lines.push(Line::from(Span::styled(
                        diff_line.content,
                        Style::default().fg(style::palette::accent()),
                    )));

                    continue;
                }
                DiffLineKind::Addition => ("+", Self::addition_line_style()),
                DiffLineKind::Deletion => ("-", Self::deletion_line_style()),
                DiffLineKind::Context => (" ", Style::default().fg(style::palette::text_muted())),
            };

            let old_str = match diff_line.old_line {
                Some(num) => format!("{num:>width$}", width = layout.gutter_width),
                None => " ".repeat(layout.gutter_width),
            };
            let new_str = match diff_line.new_line {
                Some(num) => format!("{num:>width$}", width = layout.gutter_width),
                None => " ".repeat(layout.gutter_width),
            };

            let gutter_text = format!("{old_str}│{new_str} ");
            let content_available = layout.content_width.saturating_sub(layout.prefix_width);
            let chunks = diff_util::wrap_diff_content(diff_line.content, content_available);

            for (idx, chunk) in chunks.iter().enumerate() {
                if idx == WRAPPED_CHUNK_START_INDEX {
                    lines.push(Line::from(vec![
                        Span::styled(gutter_text.clone(), gutter_style),
                        Span::styled(sign, content_style),
                        Span::styled(*chunk, content_style),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled(" ".repeat(layout.prefix_width), gutter_style),
                        Span::styled(*chunk, content_style),
                    ]));
                }
            }
        }

        if lines.is_empty() {
            lines.push(Line::from(" No changes found. "));
        }

        lines
    }

    /// Renders a slim scrollbar inside the diff panel so users can see their
    /// position in long diffs at a glance.
    fn render_diff_scrollbar(
        f: &mut Frame,
        area: Rect,
        viewport_height: u16,
        scroll_offset: u16,
        total_lines: usize,
    ) {
        if viewport_height == 0 {
            return;
        }

        if !diff_util::diff_has_scrollable_overflow(total_lines, viewport_height) {
            return;
        }

        let track_height = usize::from(viewport_height);
        let thumb_height = (track_height * track_height / total_lines).max(1);
        let max_scroll = total_lines.saturating_sub(track_height);
        let max_thumb_offset = track_height.saturating_sub(thumb_height);
        let thumb_offset = usize::from(scroll_offset)
            .saturating_mul(max_thumb_offset)
            .checked_div(max_scroll)
            .unwrap_or(0);

        let scrollbar_area = diff_util::diff_scrollbar_area(area, viewport_height);
        let mut scrollbar_lines = Vec::with_capacity(track_height);

        for line_index in 0..track_height {
            let is_thumb_line =
                line_index >= thumb_offset && line_index < thumb_offset + thumb_height;
            let (symbol, symbol_style) = if is_thumb_line {
                (
                    SCROLLBAR_THUMB_SYMBOL,
                    Style::default().fg(style::palette::warning()),
                )
            } else {
                (
                    SCROLLBAR_TRACK_SYMBOL,
                    Style::default().fg(style::palette::text_subtle()),
                )
            };

            scrollbar_lines.push(Line::from(Span::styled(symbol, symbol_style)));
        }

        f.render_widget(Paragraph::new(scrollbar_lines), scrollbar_area);
    }
}

impl Page for DiffPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let areas = diff_util::diff_page_areas(area);

        let parsed = parse_diff_lines(&self.diff);
        let tree_items = FileExplorer::file_tree_items(&parsed);

        FileExplorer::new(&parsed)
            .selected_index(self.file_explorer_selected_index)
            .render(f, areas.file_list_area);

        match self.right_panel {
            DiffRightPanel::Diff => {
                let filtered =
                    selected_diff_lines(&parsed, &tree_items, self.file_explorer_selected_index);
                self.render_diff_content(
                    f,
                    areas.diff_area,
                    &filtered,
                    self.session.stats.added_lines,
                    self.session.stats.deleted_lines,
                );
            }
            DiffRightPanel::Comments => {
                self.render_comments_content(f, areas.diff_area);
            }
        }

        let help_message = Paragraph::new(help_action::footer_line(
            &help_action::diff_footer_actions(self.right_panel),
        ));
        f.render_widget(help_message, areas.footer_area);
    }
}

impl DiffPage<'_> {
    /// Renders the cached review-request comments into the right panel.
    ///
    /// Threads are stacked with a `path:line` header and markdown-rendered
    /// comment bodies. Pull-request-level conversation comments are shown at
    /// the top under a "General discussion" header.
    fn render_comments_content(&self, f: &mut Frame, area: Rect) {
        let inner_width = area.width.saturating_sub(2);
        let (thread_count, comment_count) = comment_counts(self.review_comment_snapshot);
        let lines = self.build_comments_lines(inner_width);
        let wrapped_lines = wrap_lines_to_rows(lines, inner_width);
        let viewport_height = area.height.saturating_sub(2);
        let scroll_offset = diff_util::clamp_diff_scroll_offset(
            self.scroll_offset,
            wrapped_lines.len(),
            viewport_height,
        );
        let title = Line::from(vec![
            Span::styled(" (", Style::default().fg(style::palette::warning())),
            Span::styled(
                format!("{comment_count}"),
                Style::default().fg(style::palette::success()),
            ),
            Span::styled(" in ", Style::default().fg(style::palette::warning())),
            Span::styled(
                format!("{thread_count}"),
                Style::default().fg(style::palette::success()),
            ),
            Span::styled(
                format!(
                    ") Comments — {} ",
                    inline_text(self.session.display_title())
                ),
                Style::default().fg(style::palette::warning()),
            ),
        ]);
        let paragraph = Paragraph::new(wrapped_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(style::border_style()),
            )
            .scroll((scroll_offset, 0));

        f.render_widget(paragraph, area);
    }

    /// Builds the line sequence that fills the comments right panel.
    fn build_comments_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        let review_request = self.session.review_request.as_ref();
        let snapshot = self.review_comment_snapshot;

        lines.push(comments_header_line(review_request, snapshot));
        lines.push(Line::default());

        match (review_request, snapshot) {
            (None, _) => {
                lines.push(empty_muted_line("Open a review request to see comments."));
            }
            (Some(review_request), None)
                if review_request.summary.state != ReviewRequestState::Open =>
            {
                lines.push(empty_closed_review_request_line(
                    review_request.summary.state,
                ));
            }
            (Some(_), None) => {
                lines.push(empty_muted_line(
                    "Waiting for review-request comments to sync...",
                ));
            }
            (Some(review_request), Some(snapshot)) => {
                if review_request.summary.state != ReviewRequestState::Open {
                    lines.push(empty_closed_review_request_line(
                        review_request.summary.state,
                    ));
                    lines.push(Line::default());
                }

                let has_pr_level = !snapshot.pr_level_comments.is_empty();
                let has_threads = !snapshot.threads.is_empty();

                if !has_pr_level && !has_threads {
                    lines.push(empty_muted_line("No review comments yet."));
                } else {
                    if has_pr_level {
                        append_pr_level_comments(
                            &mut lines,
                            &snapshot.pr_level_comments,
                            self.markdown_render_cache,
                            width,
                        );
                    }
                    for (index, thread) in snapshot.threads.iter().enumerate() {
                        if index > 0 || has_pr_level {
                            lines.push(Line::default());
                        }
                        append_thread_lines(&mut lines, thread, self.markdown_render_cache, width);
                    }
                }
            }
        }

        lines
    }
}

/// Renders a muted one-line empty state.
fn empty_muted_line(text: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        text,
        Style::default().fg(style::palette::text_muted()),
    ))
}

/// Returns the top-of-panel summary line for review comments.
fn comments_header_line(
    review_request: Option<&ReviewRequest>,
    snapshot: Option<&ReviewCommentSnapshot>,
) -> Line<'static> {
    let label = match review_request {
        Some(review_request) => format!("Review request {}", review_request.summary.display_id),
        None => "Review Comments".to_string(),
    };
    let (thread_count, comment_count) = comment_counts(snapshot);

    Line::from(vec![
        Span::styled(
            label,
            Style::default()
                .fg(style::palette::text())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" — {comment_count} comments in {thread_count} threads"),
            Style::default().fg(style::palette::text_muted()),
        ),
    ])
}

/// Returns `(thread_count, comment_count)` totals for a review-comment
/// snapshot, combining inline-thread comments with pull-request-level
/// conversation comments.
fn comment_counts(snapshot: Option<&ReviewCommentSnapshot>) -> (usize, usize) {
    snapshot.map_or((0, 0), |snapshot| {
        let thread_count = snapshot.threads.len();
        let thread_comment_count = snapshot
            .threads
            .iter()
            .map(|thread| thread.comments.len())
            .sum::<usize>();
        let comment_count = snapshot.pr_level_comments.len() + thread_comment_count;

        (thread_count, comment_count)
    })
}

/// Returns an empty-state line for a merged or closed review request.
fn empty_closed_review_request_line(state: ReviewRequestState) -> Line<'static> {
    let label = match state {
        ReviewRequestState::Merged => {
            "This review request was merged; no further comments will sync."
        }
        ReviewRequestState::Closed => {
            "This review request was closed; no further comments will sync."
        }
        ReviewRequestState::Open => "",
    };

    Line::from(Span::styled(
        label.to_string(),
        Style::default().fg(style::palette::text_muted()),
    ))
}

/// Appends a "General discussion" section containing pull-request-level
/// conversation comments.
fn append_pr_level_comments(
    lines: &mut Vec<Line<'static>>,
    comments: &[ReviewComment],
    markdown_render_cache: &markdown::MarkdownRenderCache,
    width: u16,
) {
    lines.push(Line::from(Span::styled(
        "General discussion".to_string(),
        Style::default()
            .fg(style::palette::text())
            .add_modifier(Modifier::BOLD),
    )));

    for (comment_index, comment) in comments.iter().enumerate() {
        if comment_index > 0 {
            lines.push(Line::default());
        }

        append_comment_body(lines, comment, markdown_render_cache, width);
    }
}

/// Appends the header and comment bodies for one inline review thread.
fn append_thread_lines(
    lines: &mut Vec<Line<'static>>,
    thread: &ReviewCommentThread,
    markdown_render_cache: &markdown::MarkdownRenderCache,
    width: u16,
) {
    lines.push(thread_header_line(thread));

    for (comment_index, comment) in thread.comments.iter().enumerate() {
        if comment_index > 0 {
            lines.push(Line::default());
        }

        append_comment_body(lines, comment, markdown_render_cache, width);
    }
}

/// Appends one comment's author header followed by the markdown-rendered body.
fn append_comment_body(
    lines: &mut Vec<Line<'static>>,
    comment: &ReviewComment,
    markdown_render_cache: &markdown::MarkdownRenderCache,
    width: u16,
) {
    lines.push(Line::from(Span::styled(
        comment.author.clone(),
        Style::default()
            .fg(style::palette::text())
            .add_modifier(Modifier::BOLD),
    )));

    let body_width = width.saturating_sub(2).max(1) as usize;
    let rendered = markdown_render_cache.render(&comment.body, body_width);
    append_indented_markdown(lines, &rendered);
}

/// Renders the `path:line · N comments · resolved/unresolved` header for one
/// inline thread.
fn thread_header_line(thread: &ReviewCommentThread) -> Line<'static> {
    let anchor = match thread.line {
        Some(line) => format!("{}:{}", thread.path, line),
        None => thread.path.clone(),
    };
    let comment_count = thread.comments.len();
    let resolution_tag = if thread.is_resolved {
        "resolved"
    } else {
        "unresolved"
    };
    let header_style = if thread.is_resolved {
        Style::default().fg(style::palette::text_muted())
    } else {
        Style::default()
            .fg(style::palette::text())
            .add_modifier(Modifier::BOLD)
    };

    Line::from(vec![
        Span::styled(anchor, header_style),
        Span::styled(
            format!("  ·  {comment_count} comments  ·  {resolution_tag}"),
            Style::default().fg(style::palette::text_muted()),
        ),
    ])
}

/// Appends rendered markdown lines indented two spaces under a comment header.
fn append_indented_markdown(lines: &mut Vec<Line<'static>>, rendered: &Arc<[Line<'static>]>) {
    for source_line in rendered.iter() {
        let mut spans = Vec::with_capacity(source_line.spans.len() + 1);
        spans.push(Span::raw("  "));
        spans.extend(source_line.spans.iter().cloned());
        lines.push(Line::from(spans));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::session::tests::SessionFixtureBuilder;
    use crate::domain::theme::ColorTheme;
    use crate::ui::util::parse_diff_lines;

    const SAMPLE_DIFF: &str = concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "+added in main\n",
        "diff --git a/README.md b/README.md\n",
        "+added in readme\n"
    );

    fn session_fixture() -> Session {
        SessionFixtureBuilder::new()
            .title(Some("Diff Session".to_string()))
            .build()
    }

    fn new_diff_page<'a>(
        session: &'a Session,
        diff: String,
        scroll_offset: u16,
        file_explorer_selected_index: usize,
        markdown_render_cache: &'a markdown::MarkdownRenderCache,
    ) -> DiffPage<'a> {
        DiffPage::new(
            session,
            diff,
            scroll_offset,
            file_explorer_selected_index,
            DiffRightPanel::Diff,
            markdown_render_cache,
            None,
        )
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    fn background_cell_count(
        buffer: &ratatui::buffer::Buffer,
        color: ratatui::style::Color,
    ) -> usize {
        buffer
            .content()
            .iter()
            .filter(|cell| cell.bg == color)
            .count()
    }

    fn foreground_symbol_cell_count(buffer: &ratatui::buffer::Buffer, symbol: &str) -> usize {
        buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() == symbol && cell.fg == style::palette::border())
            .count()
    }

    #[test]
    fn test_render_shows_updated_diff_help_hint() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let mut session = session_fixture();
        session.stats.added_lines = 1;
        session.stats.deleted_lines = 0;
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let mut diff_page = new_diff_page(
            &session,
            "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            0,
            0,
            &markdown_render_cache,
        );
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut diff_page, frame, area);
            })
            .expect("failed to draw diff page");

        // Assert
        let buffer = terminal.backend().buffer();
        let text = buffer_text(buffer);
        assert!(text.contains("(+1 -0) Diff — Diff Session"));
        assert!(text.contains("j/k: select file"));
        assert!(text.contains("c: Show comments"));
        assert!(foreground_symbol_cell_count(buffer, "┌") >= 2);
    }

    #[test]
    fn test_render_diff_title_uses_persisted_session_line_totals() {
        // Arrange
        let mut session = session_fixture();
        session.stats.added_lines = 9;
        session.stats.deleted_lines = 4;
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let mut diff_page = new_diff_page(
            &session,
            "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            0,
            0,
            &markdown_render_cache,
        );
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut diff_page, frame, area);
            })
            .expect("failed to draw diff page");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("(+9 -4) Diff — Diff Session"));
        assert!(!text.contains("(+1 -0) Diff — Diff Session"));
    }

    #[test]
    fn test_selected_diff_lines_returns_filtered_section_for_selected_file() {
        // Arrange
        let parsed_lines = parse_diff_lines(SAMPLE_DIFF);
        let tree_items = FileExplorer::file_tree_items(&parsed_lines);

        // Act
        let selected_lines = selected_diff_lines(&parsed_lines, &tree_items, 1);

        // Assert
        assert_eq!(selected_lines.len(), 2);
        assert_eq!(
            selected_lines[0].content,
            "diff --git a/src/main.rs b/src/main.rs"
        );
        assert_eq!(selected_lines[1].content, "added in main");
    }

    #[test]
    fn test_selected_diff_lines_returns_full_diff_when_index_is_out_of_bounds() {
        // Arrange
        let parsed_lines = parse_diff_lines(SAMPLE_DIFF);
        let tree_items = FileExplorer::file_tree_items(&parsed_lines);

        // Act
        let selected_lines = selected_diff_lines(&parsed_lines, &tree_items, usize::MAX);

        // Assert
        assert_eq!(selected_lines.len(), parsed_lines.len());
        assert_eq!(selected_lines[0].content, parsed_lines[0].content);
        assert_eq!(selected_lines[3].content, parsed_lines[3].content);
    }

    #[test]
    fn test_render_applies_background_tints_to_changed_lines() {
        // Arrange
        let session = session_fixture();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let mut diff_page = new_diff_page(
            &session,
            concat!(
                "diff --git a/src/main.rs b/src/main.rs\n",
                "@@ -1,2 +1,2 @@\n",
                "-old content\n",
                "+new content\n"
            )
            .to_string(),
            0,
            0,
            &markdown_render_cache,
        );
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut diff_page, frame, area);
            })
            .expect("failed to draw diff page");

        // Assert
        let buffer = terminal.backend().buffer();
        assert!(
            background_cell_count(buffer, style::palette::surface_success()) > 0,
            "expected added lines to include success background tint"
        );
        assert!(
            background_cell_count(buffer, style::palette::surface_danger()) > 0,
            "expected removed lines to include danger background tint"
        );
    }

    #[test]
    fn test_render_shows_scrollbar_for_overflowing_diff() {
        // Arrange
        let session = session_fixture();
        let diff = (0..80)
            .map(|index| format!("+line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let mut diff_page = new_diff_page(&session, diff, 12, 0, &markdown_render_cache);
        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut diff_page, frame, area);
            })
            .expect("failed to draw diff page");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains(SCROLLBAR_TRACK_SYMBOL));
        assert!(text.contains(SCROLLBAR_THUMB_SYMBOL));
    }

    #[test]
    fn test_render_clamps_overscroll_to_last_visible_diff_lines() {
        // Arrange
        let session = session_fixture();
        let diff = (0..40)
            .map(|index| format!("+line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let mut diff_page = new_diff_page(&session, diff, u16::MAX, 0, &markdown_render_cache);
        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut diff_page, frame, area);
            })
            .expect("failed to draw diff page");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("line 39"));
    }
}
