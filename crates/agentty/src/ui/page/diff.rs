use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::Hasher;
use std::sync::Arc;

use ag_forge::{
    ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewCommentThread,
    ReviewRequestState,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use rustc_hash::FxHasher;

use crate::domain::session::{ReviewRequest, Session};
use crate::infra::review_comment_cache::CachedReviewCommentSnapshot;
use crate::ui::component::file_explorer::{FileExplorer, FileTreeItem};
use crate::ui::diff_util::{
    DiffLine, DiffLineKind, diff_header_new_path, diff_header_old_path, parse_diff_lines,
};
use crate::ui::state::app_mode::DiffRightPanel;
use crate::ui::state::help_action;
use crate::ui::text_util::{inline_text, wrap_lines_to_rows};
use crate::ui::{Component, Page, diff_util, markdown, style};

const SCROLLBAR_TRACK_SYMBOL: &str = "│";
const SCROLLBAR_THUMB_SYMBOL: &str = "█";
const WRAPPED_CHUNK_START_INDEX: usize = 0;
const DIFF_CONTENT_CACHE_ENTRY_LIMIT: usize = 8;
const DIFF_LAYOUT_CACHE_ENTRY_LIMIT: usize = 16;

/// Compact identity for one raw diff string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiffContentCacheKey {
    content_hash: u64,
    content_len: usize,
}

/// Cache key for one fully assembled diff-panel layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiffLayoutCacheKey {
    diff_area_height: u16,
    diff_area_width: u16,
    diff_content: DiffContentCacheKey,
    markdown_style_version: u64,
    reserve_scrollbar_width: bool,
    review_comment_version: u64,
    selected_index: usize,
}

/// Owned diff line retained by the parsed diff cache.
#[derive(Clone, Debug, Eq, PartialEq)]
struct OwnedDiffLine {
    content: String,
    kind: DiffLineKind,
    new_line: Option<u32>,
    old_line: Option<u32>,
}

impl OwnedDiffLine {
    /// Copies one borrowed parsed diff line into the content cache.
    fn from_diff_line(diff_line: DiffLine<'_>) -> Self {
        Self {
            content: diff_line.content.to_string(),
            kind: diff_line.kind,
            new_line: diff_line.new_line,
            old_line: diff_line.old_line,
        }
    }

    /// Returns this cached line as the borrowed representation expected by
    /// existing diff formatting helpers.
    fn borrowed(&self) -> DiffLine<'_> {
        DiffLine {
            content: &self.content,
            kind: self.kind,
            new_line: self.new_line,
            old_line: self.old_line,
        }
    }
}

/// Parsed diff data reused by file-tree rendering and diff layout assembly.
#[derive(Clone)]
pub(crate) struct DiffContentSnapshot {
    file_list_lines: Arc<[Line<'static>]>,
    key: DiffContentCacheKey,
    parsed_lines: Arc<[OwnedDiffLine]>,
    tree_items: Arc<[FileTreeItem]>,
}

impl DiffContentSnapshot {
    /// Returns cached file-explorer lines for the left diff panel.
    pub(crate) fn file_list_lines(&self) -> Arc<[Line<'static>]> {
        Arc::clone(&self.file_list_lines)
    }

    /// Returns the number of selectable file-tree entries in this diff.
    pub(crate) fn item_count(&self) -> usize {
        self.tree_items.len()
    }

    /// Returns parsed lines for the active file-tree selection.
    fn selected_lines(&self, selected_index: usize) -> Vec<DiffLine<'_>> {
        let parsed_lines = self.borrowed_lines();
        let Some(selected_item) = self.tree_items.get(selected_index) else {
            return parsed_lines;
        };

        FileExplorer::filter_diff_lines(&parsed_lines, selected_item)
    }

    /// Returns the complete parsed diff as borrowed lines.
    fn borrowed_lines(&self) -> Vec<DiffLine<'_>> {
        self.parsed_lines
            .iter()
            .map(OwnedDiffLine::borrowed)
            .collect()
    }
}

/// Cached fully assembled diff lines for one render-affecting key.
#[derive(Clone)]
struct DiffCachedLayout {
    line_count: usize,
    lines: Arc<[Line<'static>]>,
    render_layout: diff_util::DiffRenderLayout,
}

/// Borrowed inputs used to derive or look up one cached diff layout.
#[derive(Clone, Copy)]
struct DiffLayoutRequest<'a> {
    content: &'a DiffContentSnapshot,
    diff_area: Rect,
    markdown_render_cache: &'a markdown::MarkdownRenderCache,
    reserve_scrollbar_width: bool,
    review_comment_snapshot: Option<&'a ReviewCommentSnapshot>,
    review_comment_version: u64,
    selected_index: usize,
}

/// Final diff layout selected for the current panel and scrollbar state.
#[derive(Clone)]
pub(crate) struct DiffResolvedLayout {
    pub(crate) line_count: usize,
    pub(crate) lines: Arc<[Line<'static>]>,
    pub(crate) render_layout: diff_util::DiffRenderLayout,
    pub(crate) show_scrollbar: bool,
}

/// Cached parsed diff snapshot entry.
struct DiffContentCacheEntry {
    key: DiffContentCacheKey,
    snapshot: DiffContentSnapshot,
}

/// Cached rendered diff layout entry.
struct DiffLayoutCacheEntry {
    key: DiffLayoutCacheKey,
    layout: DiffCachedLayout,
}

/// Bounded cache for parsed diff content and fully assembled diff layouts.
///
/// The parsed-content layer avoids re-parsing the same raw diff and rebuilding
/// file-tree metadata on every frame. The rendered-layout layer sits above
/// markdown rendering and inline comment lookup so scroll metrics and frame
/// painting reuse the same styled rows for unchanged diff content, selection,
/// panel width/height, scrollbar gutter state, review-comment version, and
/// active style version.
pub struct DiffLayoutCache {
    content_entries: RefCell<VecDeque<DiffContentCacheEntry>>,
    layout_entries: RefCell<VecDeque<DiffLayoutCacheEntry>>,
}

impl Default for DiffLayoutCache {
    fn default() -> Self {
        Self {
            content_entries: RefCell::new(VecDeque::with_capacity(DIFF_CONTENT_CACHE_ENTRY_LIMIT)),
            layout_entries: RefCell::new(VecDeque::with_capacity(DIFF_LAYOUT_CACHE_ENTRY_LIMIT)),
        }
    }
}

impl DiffLayoutCache {
    /// Returns parsed diff and file-tree data from cache or derives it once.
    pub(crate) fn content(&self, diff: &str) -> DiffContentSnapshot {
        let key = Self::content_cache_key(diff);
        if let Some(snapshot) = self.cached_content(key) {
            return snapshot;
        }

        let parsed_lines = parse_diff_lines(diff);
        let (file_list_lines, tree_items) = FileExplorer::file_tree(&parsed_lines);
        let snapshot = DiffContentSnapshot {
            file_list_lines: Arc::from(file_list_lines),
            key,
            parsed_lines: Arc::from(
                parsed_lines
                    .into_iter()
                    .map(OwnedDiffLine::from_diff_line)
                    .collect::<Vec<_>>(),
            ),
            tree_items: Arc::from(tree_items),
        };
        self.store_content(DiffContentCacheEntry {
            key,
            snapshot: snapshot.clone(),
        });

        snapshot
    }

    /// Returns the resolved diff layout for the current panel, using cached
    /// no-scrollbar line count to decide whether a gutter-reserved layout is
    /// required.
    pub(crate) fn resolved_layout(
        &self,
        content: &DiffContentSnapshot,
        selected_index: usize,
        diff_area: Rect,
        review_comment_snapshot: Option<&CachedReviewCommentSnapshot>,
        markdown_render_cache: &markdown::MarkdownRenderCache,
    ) -> DiffResolvedLayout {
        let review_comment_version = review_comment_snapshot
            .map(CachedReviewCommentSnapshot::version)
            .unwrap_or_default();
        let review_comment_snapshot =
            review_comment_snapshot.map(CachedReviewCommentSnapshot::snapshot);
        let layout_without_scrollbar = self.layout(DiffLayoutRequest {
            content,
            diff_area,
            markdown_render_cache,
            reserve_scrollbar_width: false,
            review_comment_snapshot,
            review_comment_version,
            selected_index,
        });
        let show_scrollbar = diff_util::diff_has_scrollable_overflow(
            layout_without_scrollbar.line_count,
            layout_without_scrollbar.render_layout.viewport_height,
        );
        if !show_scrollbar {
            return DiffResolvedLayout {
                line_count: layout_without_scrollbar.line_count,
                lines: layout_without_scrollbar.lines,
                render_layout: layout_without_scrollbar.render_layout,
                show_scrollbar: false,
            };
        }

        let layout_with_scrollbar = self.layout(DiffLayoutRequest {
            content,
            diff_area,
            markdown_render_cache,
            reserve_scrollbar_width: true,
            review_comment_snapshot,
            review_comment_version,
            selected_index,
        });
        let show_scrollbar = diff_util::diff_has_scrollable_overflow(
            layout_with_scrollbar.line_count,
            layout_with_scrollbar.render_layout.viewport_height,
        );

        DiffResolvedLayout {
            line_count: layout_with_scrollbar.line_count,
            lines: layout_with_scrollbar.lines,
            render_layout: layout_with_scrollbar.render_layout,
            show_scrollbar,
        }
    }

    /// Returns cached parsed content for a matching diff fingerprint and
    /// promotes the entry to the front of the LRU queue.
    fn cached_content(&self, key: DiffContentCacheKey) -> Option<DiffContentSnapshot> {
        let mut entries = self.content_entries.borrow_mut();
        let entry_index = entries.iter().position(|entry| entry.key == key)?;
        let entry = entries.remove(entry_index)?;
        let snapshot = entry.snapshot.clone();
        entries.push_front(entry);

        Some(snapshot)
    }

    /// Stores one parsed-content entry and evicts the oldest entry when the
    /// bounded capacity is exceeded.
    fn store_content(&self, entry: DiffContentCacheEntry) {
        let mut entries = self.content_entries.borrow_mut();
        entries.push_front(entry);

        while entries.len() > DIFF_CONTENT_CACHE_ENTRY_LIMIT {
            entries.pop_back();
        }
    }

    /// Returns cached rendered diff rows, or assembles and stores them when
    /// any render-affecting input changed.
    fn layout(&self, request: DiffLayoutRequest<'_>) -> DiffCachedLayout {
        let DiffLayoutRequest {
            content,
            diff_area,
            markdown_render_cache,
            reserve_scrollbar_width,
            review_comment_snapshot,
            review_comment_version,
            selected_index,
        } = request;
        let key = DiffLayoutCacheKey {
            diff_area_height: diff_area.height,
            diff_area_width: diff_area.width,
            diff_content: content.key,
            markdown_style_version: Self::markdown_style_version(markdown_render_cache),
            reserve_scrollbar_width,
            review_comment_version,
            selected_index,
        };
        if let Some(layout) = self.cached_layout(&key) {
            return layout;
        }

        let selected_lines = content.selected_lines(selected_index);
        let render_layout =
            diff_util::diff_render_layout(&selected_lines, diff_area, reserve_scrollbar_width);
        let lines = build_diff_lines_with_comments(
            &selected_lines,
            render_layout,
            review_comment_snapshot,
            markdown_render_cache,
        );
        let layout = DiffCachedLayout {
            line_count: lines.len(),
            lines: Arc::from(lines),
            render_layout,
        };
        self.store_layout(DiffLayoutCacheEntry {
            key,
            layout: layout.clone(),
        });

        layout
    }

    /// Returns cached rendered layout for a matching entry and promotes it to
    /// the front of the LRU queue.
    fn cached_layout(&self, key: &DiffLayoutCacheKey) -> Option<DiffCachedLayout> {
        let mut entries = self.layout_entries.borrow_mut();
        let entry_index = entries.iter().position(|entry| &entry.key == key)?;
        let entry = entries.remove(entry_index)?;
        let layout = entry.layout.clone();
        entries.push_front(entry);

        Some(layout)
    }

    /// Stores one rendered layout and evicts the oldest entries over the
    /// bounded capacity.
    fn store_layout(&self, entry: DiffLayoutCacheEntry) {
        let mut entries = self.layout_entries.borrow_mut();
        entries.push_front(entry);

        while entries.len() > DIFF_LAYOUT_CACHE_ENTRY_LIMIT {
            entries.pop_back();
        }
    }

    /// Returns a compact key for the raw diff string.
    fn content_cache_key(diff: &str) -> DiffContentCacheKey {
        let mut hasher = FxHasher::default();
        hasher.write(diff.as_bytes());

        DiffContentCacheKey {
            content_hash: hasher.finish(),
            content_len: diff.len(),
        }
    }

    /// Returns the style version that invalidates cached styled diff rows.
    fn markdown_style_version(markdown_render_cache: &markdown::MarkdownRenderCache) -> u64 {
        markdown_render_cache
            .version()
            .wrapping_add(style::active_theme_cache_version())
    }
}

/// Renders the current session's git diff in a scrollable page.
///
/// The right-hand panel shows either the raw git diff or cached review
/// comments, selected by [`DiffPage::right_panel`]. The left-hand file explorer
/// is unchanged across panels; the `c` key toggles the right-hand view.
pub struct DiffPage<'a> {
    pub diff: &'a str,
    pub diff_layout_cache: &'a DiffLayoutCache,
    pub file_explorer_selected_index: usize,
    pub markdown_render_cache: &'a markdown::MarkdownRenderCache,
    pub review_comment_snapshot: Option<&'a CachedReviewCommentSnapshot>,
    pub right_panel: DiffRightPanel,
    pub scroll_offset: u16,
    pub session: &'a Session,
}

/// Borrowed inputs required to construct a [`DiffPage`] for one frame.
#[derive(Clone, Copy)]
pub struct DiffPageInput<'a> {
    /// Raw unified diff currently shown by the page.
    pub diff: &'a str,
    /// Shared cache for parsed diff content and rendered diff layouts.
    pub diff_layout_cache: &'a DiffLayoutCache,
    /// Selected file-tree row in the left panel.
    pub file_explorer_selected_index: usize,
    /// Shared markdown render cache used for inline review comments.
    pub markdown_render_cache: &'a markdown::MarkdownRenderCache,
    /// Cached review-comment snapshot and version for inline diff comments.
    pub review_comment_snapshot: Option<&'a CachedReviewCommentSnapshot>,
    /// Active right-hand panel selection.
    pub right_panel: DiffRightPanel,
    /// Vertical scroll offset inside the active right panel.
    pub scroll_offset: u16,
    /// Session whose diff is being rendered.
    pub session: &'a Session,
}

impl<'a> DiffPage<'a> {
    /// Creates a diff page for the given session, scroll position, and right
    /// panel selection.
    pub fn new(input: DiffPageInput<'a>) -> Self {
        let DiffPageInput {
            diff,
            diff_layout_cache,
            file_explorer_selected_index,
            markdown_render_cache,
            review_comment_snapshot,
            right_panel,
            scroll_offset,
            session,
        } = input;

        Self {
            diff,
            diff_layout_cache,
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
        content: &DiffContentSnapshot,
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

        let layout = self.diff_layout_cache.resolved_layout(
            content,
            self.file_explorer_selected_index,
            area,
            self.review_comment_snapshot,
            self.markdown_render_cache,
        );

        let scroll_offset = diff_util::clamp_diff_scroll_offset(
            self.scroll_offset,
            layout.line_count,
            layout.render_layout.viewport_height,
        );
        let paint_lines = Self::borrowed_visible_lines(
            &layout.lines,
            scroll_offset,
            layout.render_layout.viewport_height,
        );

        let paragraph = Paragraph::new(paint_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(style::border_style()),
        );

        f.render_widget(paragraph, area);

        if layout.show_scrollbar {
            Self::render_diff_scrollbar(
                f,
                area,
                layout.render_layout.viewport_height,
                scroll_offset,
                layout.line_count,
            );
        }
    }

    /// Builds short-lived paint rows for the visible viewport slice, borrowing
    /// span content from cached static diff rows instead of cloning the whole
    /// diff on every scroll repaint.
    fn borrowed_visible_lines<'line>(
        lines: &'line [Line<'static>],
        scroll_offset: u16,
        viewport_height: u16,
    ) -> Vec<Line<'line>> {
        let start_index = usize::from(scroll_offset).min(lines.len());
        let end_index = start_index
            .saturating_add(usize::from(viewport_height))
            .min(lines.len());

        lines[start_index..end_index]
            .iter()
            .map(|line| Line {
                alignment: line.alignment,
                spans: line.spans.iter().map(Self::borrowed_paint_span).collect(),
                style: line.style,
            })
            .collect()
    }

    /// Builds one borrowed paint span from a cached diff span.
    fn borrowed_paint_span<'span>(span: &'span Span<'static>) -> Span<'span> {
        Span {
            content: Cow::Borrowed(span.content.as_ref()),
            style: span.style,
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
    fn build_diff_lines(
        parsed: &[DiffLine<'_>],
        layout: diff_util::DiffRenderLayout,
        comment_index: &InlineCommentIndex<'_>,
        markdown_render_cache: &markdown::MarkdownRenderCache,
    ) -> Vec<Line<'static>> {
        let gutter_style = Style::default().fg(style::palette::text_subtle());
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(parsed.len());
        let mut current_new_path: Option<&str> = None;
        let mut current_old_path: Option<&str> = None;

        for diff_line in parsed {
            let (sign, content_style) = match diff_line.kind {
                DiffLineKind::FileHeader => {
                    if diff_line.content.starts_with("diff ") && !lines.is_empty() {
                        lines.push(Line::from(""));
                    }
                    if diff_line.content.starts_with("diff ") {
                        current_new_path = diff_header_new_path(diff_line.content);
                        current_old_path = diff_header_old_path(diff_line.content);
                    }
                    lines.push(Line::from(Span::styled(
                        diff_line.content.to_string(),
                        Style::default().fg(style::palette::warning()),
                    )));
                    append_inline_comments_for_diff_line(
                        &mut lines,
                        comment_index,
                        markdown_render_cache,
                        layout.content_width,
                        current_new_path,
                        current_old_path,
                        diff_line,
                    );

                    continue;
                }
                DiffLineKind::HunkHeader => {
                    lines.push(Line::from(Span::styled(
                        diff_line.content.to_string(),
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
                        Span::styled((*chunk).to_string(), content_style),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled(" ".repeat(layout.prefix_width), gutter_style),
                        Span::styled((*chunk).to_string(), content_style),
                    ]));
                }
            }
            append_inline_comments_for_diff_line(
                &mut lines,
                comment_index,
                markdown_render_cache,
                layout.content_width,
                current_new_path,
                current_old_path,
                diff_line,
            );
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

/// Returns the max valid scroll offset for a diff panel that may include
/// inline review-request comments.
pub(crate) fn diff_view_max_scroll_offset_with_comments(
    diff: &str,
    selected_index: usize,
    terminal_area: Rect,
    review_comment_snapshot: Option<&CachedReviewCommentSnapshot>,
    markdown_render_cache: &markdown::MarkdownRenderCache,
    diff_layout_cache: &DiffLayoutCache,
) -> u16 {
    let diff_area = diff_util::diff_page_areas(terminal_area).diff_area;
    let content = diff_layout_cache.content(diff);
    let layout = diff_layout_cache.resolved_layout(
        &content,
        selected_index,
        diff_area,
        review_comment_snapshot,
        markdown_render_cache,
    );
    if layout.render_layout.viewport_height == 0 {
        return 0;
    }

    diff_util::clamp_diff_scroll_offset(
        u16::MAX,
        layout.line_count,
        layout.render_layout.viewport_height,
    )
}

/// Builds diff panel rows after merging inline comment threads onto matching
/// diff anchors.
fn build_diff_lines_with_comments(
    parsed: &[DiffLine<'_>],
    layout: diff_util::DiffRenderLayout,
    review_comment_snapshot: Option<&ReviewCommentSnapshot>,
    markdown_render_cache: &markdown::MarkdownRenderCache,
) -> Vec<Line<'static>> {
    let comment_index = InlineCommentIndex::new(review_comment_snapshot);

    DiffPage::build_diff_lines(parsed, layout, &comment_index, markdown_render_cache)
}

/// Hash key used to map comment threads to one concrete diff side and line.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct InlineCommentKey {
    line: u32,
    path: String,
    side: ReviewCommentAnchorSide,
}

/// Side-aware lookup table for rendering unresolved review threads below
/// matching diff lines.
struct InlineCommentIndex<'a> {
    file_threads: HashMap<String, Vec<&'a ReviewCommentThread>>,
    line_threads: HashMap<InlineCommentKey, Vec<&'a ReviewCommentThread>>,
}

impl<'a> InlineCommentIndex<'a> {
    /// Builds a lookup table from one cached review-comment snapshot, excluding
    /// resolved threads from all inline diff anchors.
    fn new(snapshot: Option<&'a ReviewCommentSnapshot>) -> Self {
        let mut index = Self {
            file_threads: HashMap::new(),
            line_threads: HashMap::new(),
        };
        let Some(snapshot) = snapshot else {
            return index;
        };

        for thread in snapshot
            .threads
            .iter()
            .filter(|thread| is_visible_thread(thread))
        {
            if thread.anchor_side == ReviewCommentAnchorSide::File || thread.line.is_none() {
                index
                    .file_threads
                    .entry(thread.path.clone())
                    .or_default()
                    .push(thread);

                continue;
            }

            let Some(line) = thread.line else {
                continue;
            };
            let key = InlineCommentKey {
                line,
                path: thread.path.clone(),
                side: thread.anchor_side,
            };
            index.line_threads.entry(key).or_default().push(thread);
        }

        index
    }

    /// Returns file-level threads anchored to `path`.
    fn file_threads(&self, path: Option<&str>) -> Vec<&'a ReviewCommentThread> {
        let Some(path) = path else {
            return Vec::new();
        };

        self.file_threads.get(path).cloned().unwrap_or_default()
    }

    /// Returns line-level threads anchored to `path`, `side`, and `line`.
    fn line_threads(
        &self,
        path: Option<&str>,
        side: ReviewCommentAnchorSide,
        line: Option<u32>,
    ) -> Vec<&'a ReviewCommentThread> {
        let (Some(path), Some(line)) = (path, line) else {
            return Vec::new();
        };
        let key = InlineCommentKey {
            line,
            path: path.to_string(),
            side,
        };

        self.line_threads.get(&key).cloned().unwrap_or_default()
    }
}

impl Page for DiffPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let areas = diff_util::diff_page_areas(area);
        let content = self.diff_layout_cache.content(self.diff);

        FileExplorer::from_cached_lines(content.file_list_lines())
            .selected_index(self.file_explorer_selected_index)
            .render(f, areas.file_list_area);

        match self.right_panel {
            DiffRightPanel::Diff => {
                self.render_diff_content(
                    f,
                    areas.diff_area,
                    &content,
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

/// Appends inline review threads anchored to `diff_line`, if any.
fn append_inline_comments_for_diff_line(
    lines: &mut Vec<Line<'static>>,
    comment_index: &InlineCommentIndex<'_>,
    markdown_render_cache: &markdown::MarkdownRenderCache,
    width: usize,
    current_new_path: Option<&str>,
    current_old_path: Option<&str>,
    diff_line: &DiffLine<'_>,
) {
    let mut threads = match diff_line.kind {
        DiffLineKind::FileHeader if diff_line.content.starts_with("diff ") => {
            let mut file_threads = comment_index.file_threads(current_new_path);
            if current_old_path != current_new_path {
                file_threads.extend(comment_index.file_threads(current_old_path));
            }

            file_threads
        }
        DiffLineKind::Addition => comment_index.line_threads(
            current_new_path,
            ReviewCommentAnchorSide::New,
            diff_line.new_line,
        ),
        DiffLineKind::Deletion => comment_index.line_threads(
            current_old_path,
            ReviewCommentAnchorSide::Old,
            diff_line.old_line,
        ),
        DiffLineKind::Context => {
            let mut context_threads = comment_index.line_threads(
                current_new_path,
                ReviewCommentAnchorSide::New,
                diff_line.new_line,
            );
            context_threads.extend(comment_index.line_threads(
                current_old_path,
                ReviewCommentAnchorSide::Old,
                diff_line.old_line,
            ));

            context_threads
        }
        DiffLineKind::FileHeader | DiffLineKind::HunkHeader => Vec::new(),
    };

    if threads.is_empty() {
        return;
    }

    threads.sort_by(|left, right| {
        anchor_side_order(left.anchor_side)
            .cmp(&anchor_side_order(right.anchor_side))
            .then_with(|| left.line.unwrap_or(0).cmp(&right.line.unwrap_or(0)))
    });

    for thread in threads {
        append_inline_thread_lines(lines, thread, markdown_render_cache, width);
    }
}

/// Returns a stable ordering for inline comment anchor sides.
fn anchor_side_order(anchor_side: ReviewCommentAnchorSide) -> u8 {
    match anchor_side {
        ReviewCommentAnchorSide::File => 0,
        ReviewCommentAnchorSide::Old => 1,
        ReviewCommentAnchorSide::New => 2,
    }
}

/// Appends one inline review thread using a compact nested visual style.
fn append_inline_thread_lines(
    lines: &mut Vec<Line<'static>>,
    thread: &ReviewCommentThread,
    markdown_render_cache: &markdown::MarkdownRenderCache,
    width: usize,
) {
    lines.push(inline_thread_header_line(thread));

    let body_width = width.saturating_sub(4).max(1);
    for comment in &thread.comments {
        append_inline_comment_body(lines, comment, markdown_render_cache, body_width);
    }
}

/// Renders the metadata row shown above one inline thread body.
fn inline_thread_header_line(thread: &ReviewCommentThread) -> Line<'static> {
    let side = match thread.anchor_side {
        ReviewCommentAnchorSide::File => "file",
        ReviewCommentAnchorSide::New => "new",
        ReviewCommentAnchorSide::Old => "old",
    };
    let status = if thread.is_resolved {
        "resolved"
    } else {
        "unresolved"
    };
    let outdated = if thread.is_outdated == Some(true) {
        " · outdated"
    } else {
        ""
    };

    Line::from(vec![
        Span::styled("    │ ", Style::default().fg(style::palette::warning())),
        Span::styled(
            format!("{} comments", thread.comments.len()),
            Style::default()
                .fg(style::palette::text())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" · {side} · {status}{outdated}"),
            Style::default().fg(style::palette::text_muted()),
        ),
    ])
}

/// Appends one inline comment author and markdown body.
fn append_inline_comment_body(
    lines: &mut Vec<Line<'static>>,
    comment: &ReviewComment,
    markdown_render_cache: &markdown::MarkdownRenderCache,
    body_width: usize,
) {
    lines.push(Line::from(vec![
        Span::styled("    │ ", Style::default().fg(style::palette::warning())),
        Span::styled(
            comment.author.clone(),
            Style::default()
                .fg(style::palette::text())
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    let rendered = markdown_render_cache.render(&comment.body, body_width);
    for source_line in rendered.iter() {
        let mut spans = Vec::with_capacity(source_line.spans.len() + 1);
        spans.push(Span::styled(
            "    │   ",
            Style::default().fg(style::palette::warning()),
        ));
        spans.extend(source_line.spans.iter().cloned());
        lines.push(Line::from(spans));
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
        let snapshot = self
            .review_comment_snapshot
            .map(CachedReviewCommentSnapshot::snapshot);
        let (thread_count, comment_count) = comment_counts(snapshot);
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
        let snapshot = self
            .review_comment_snapshot
            .map(CachedReviewCommentSnapshot::snapshot);

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
                let has_threads = snapshot.threads.iter().any(is_visible_thread);

                if !has_pr_level && !has_threads {
                    lines.push(empty_muted_line("No unresolved review comments."));
                } else {
                    if has_pr_level {
                        append_pr_level_comments(
                            &mut lines,
                            &snapshot.pr_level_comments,
                            self.markdown_render_cache,
                            width,
                        );
                    }
                    for (index, thread) in snapshot
                        .threads
                        .iter()
                        .filter(|thread| is_visible_thread(thread))
                        .enumerate()
                    {
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

/// Returns the top-of-panel summary line for unresolved review comments.
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

/// Returns unresolved `(thread_count, comment_count)` totals for a
/// review-comment snapshot, combining unresolved inline-thread comments with
/// pull-request-level conversation comments.
fn comment_counts(snapshot: Option<&ReviewCommentSnapshot>) -> (usize, usize) {
    snapshot.map_or((0, 0), |snapshot| {
        let mut thread_count = 0;
        let mut thread_comment_count = 0;
        for thread in snapshot
            .threads
            .iter()
            .filter(|thread| is_visible_thread(thread))
        {
            thread_count += 1;
            thread_comment_count += thread.comments.len();
        }
        let comment_count = snapshot.pr_level_comments.len() + thread_comment_count;

        (thread_count, comment_count)
    })
}

/// Returns whether a review thread still needs attention in the diff UI.
fn is_visible_thread(thread: &ReviewCommentThread) -> bool {
    !thread.is_resolved
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

/// Renders the `path:line · side · N comments · resolved/unresolved` header
/// for one inline thread in the overview comments panel.
fn thread_header_line(thread: &ReviewCommentThread) -> Line<'static> {
    let anchor = match thread.line {
        Some(line) => format!("{}:{}", thread.path, line),
        None => thread.path.clone(),
    };
    let side_tag = match thread.anchor_side {
        ReviewCommentAnchorSide::File => "file",
        ReviewCommentAnchorSide::New => "new",
        ReviewCommentAnchorSide::Old => "old",
    };
    let comment_count = thread.comments.len();
    let resolution_tag = if thread.is_resolved {
        "resolved"
    } else {
        "unresolved"
    };
    let outdated_tag = if thread.is_outdated == Some(true) {
        "  ·  outdated"
    } else {
        ""
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
            format!(
                "  ·  {side_tag}  ·  {comment_count} comments  ·  {resolution_tag}{outdated_tag}"
            ),
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
    use crate::domain::session::{ForgeKind, ReviewRequestSummary};
    use crate::domain::theme::ColorTheme;
    use crate::ui::diff_util::{parse_diff_lines, selected_diff_lines};

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

    fn session_with_review_request() -> Session {
        let mut session = session_fixture();
        session.review_request = Some(ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/diff-session".to_string(),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Diff Session".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            },
        });

        session
    }

    fn new_diff_page<'a>(
        session: &'a Session,
        diff: &'a str,
        scroll_offset: u16,
        file_explorer_selected_index: usize,
        markdown_render_cache: &'a markdown::MarkdownRenderCache,
    ) -> DiffPage<'a> {
        DiffPage::new(DiffPageInput {
            diff,
            diff_layout_cache: test_diff_layout_cache(),
            file_explorer_selected_index,
            markdown_render_cache,
            review_comment_snapshot: None,
            right_panel: DiffRightPanel::Diff,
            scroll_offset,
            session,
        })
    }

    fn test_diff_layout_cache() -> &'static DiffLayoutCache {
        Box::leak(Box::new(DiffLayoutCache::default()))
    }

    fn cached_review_comment_snapshot(
        snapshot: ReviewCommentSnapshot,
    ) -> CachedReviewCommentSnapshot {
        let cache = crate::infra::review_comment_cache::ReviewCommentCache::default();
        let session_id: crate::domain::session::SessionId = "cached-comment-test".into();

        cache.record_snapshot(session_id.clone(), snapshot);

        cache
            .snapshot(&session_id)
            .expect("test snapshot should be cached")
    }

    fn mixed_review_comment_snapshot() -> ReviewCommentSnapshot {
        ReviewCommentSnapshot {
            pr_level_comments: Vec::new(),
            threads: vec![
                review_thread(2, "alice", "Please handle this edge case.", false),
                review_thread(2, "bob", "Resolved and should stay hidden.", true),
            ],
        }
    }

    fn review_thread(
        line: u32,
        author: &str,
        body: &str,
        is_resolved: bool,
    ) -> ReviewCommentThread {
        ReviewCommentThread {
            anchor_side: ReviewCommentAnchorSide::New,
            comments: vec![ReviewComment {
                author: author.to_string(),
                body: body.to_string(),
            }],
            is_outdated: Some(false),
            is_resolved,
            line: Some(line),
            path: "src/main.rs".to_string(),
            start_line: None,
        }
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

    fn line_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn test_diff_layout_cache_reuses_parsed_content_snapshot() {
        // Arrange
        let cache = DiffLayoutCache::default();

        // Act
        let first_content = cache.content(SAMPLE_DIFF);
        let second_content = cache.content(SAMPLE_DIFF);

        // Assert
        assert!(Arc::ptr_eq(
            &first_content.parsed_lines,
            &second_content.parsed_lines
        ));
        assert!(Arc::ptr_eq(
            &first_content.file_list_lines,
            &second_content.file_list_lines
        ));
    }

    #[test]
    fn test_diff_layout_cache_reuses_rendered_layout_rows() {
        // Arrange
        let cache = DiffLayoutCache::default();
        let content = cache.content(SAMPLE_DIFF);
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let area = Rect::new(0, 0, 80, 12);

        // Act
        let first_layout = cache.resolved_layout(&content, 0, area, None, &markdown_render_cache);
        let second_layout = cache.resolved_layout(&content, 0, area, None, &markdown_render_cache);

        // Assert
        assert!(Arc::ptr_eq(&first_layout.lines, &second_layout.lines));
        assert_eq!(first_layout.line_count, second_layout.line_count);
    }

    #[test]
    fn test_diff_layout_cache_keys_review_comment_version() {
        // Arrange
        let cache = DiffLayoutCache::default();
        let content = cache.content(concat!(
            "diff --git a/src/main.rs b/src/main.rs\n",
            "@@ -1,2 +1,2 @@\n",
            " unchanged\n",
            "+new content\n"
        ));
        let review_cache = crate::infra::review_comment_cache::ReviewCommentCache::default();
        let session_id: crate::domain::session::SessionId = "diff-cache-comments".into();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let area = Rect::new(0, 0, 80, 12);
        review_cache.record_snapshot(
            session_id.clone(),
            ReviewCommentSnapshot {
                pr_level_comments: Vec::new(),
                threads: vec![review_thread(2, "alice", "First body.", false)],
            },
        );
        let first_snapshot = review_cache
            .snapshot(&session_id)
            .expect("first snapshot should be cached");
        review_cache.record_snapshot(
            session_id.clone(),
            ReviewCommentSnapshot {
                pr_level_comments: Vec::new(),
                threads: vec![review_thread(2, "alice", "Second body.", false)],
            },
        );
        let second_snapshot = review_cache
            .snapshot(&session_id)
            .expect("second snapshot should be cached");

        // Act
        let first_layout = cache.resolved_layout(
            &content,
            0,
            area,
            Some(&first_snapshot),
            &markdown_render_cache,
        );
        let second_layout = cache.resolved_layout(
            &content,
            0,
            area,
            Some(&second_snapshot),
            &markdown_render_cache,
        );

        // Assert
        assert_eq!(first_snapshot.version(), 1);
        assert_eq!(second_snapshot.version(), 2);
        assert!(!Arc::ptr_eq(&first_layout.lines, &second_layout.lines));
    }

    #[test]
    fn test_render_shows_updated_diff_help_hint() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let mut session = session_fixture();
        session.stats.added_lines = 1;
        session.stats.deleted_lines = 0;
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let diff = "diff --git a/src/main.rs b/src/main.rs\n+added";
        let mut diff_page = new_diff_page(&session, diff, 0, 0, &markdown_render_cache);
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
        let diff = "diff --git a/src/main.rs b/src/main.rs\n+added";
        let mut diff_page = new_diff_page(&session, diff, 0, 0, &markdown_render_cache);
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
        let diff = concat!(
            "diff --git a/src/main.rs b/src/main.rs\n",
            "@@ -1,2 +1,2 @@\n",
            "-old content\n",
            "+new content\n"
        );
        let mut diff_page = new_diff_page(&session, diff, 0, 0, &markdown_render_cache);
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
    fn test_render_shows_inline_comments_below_matching_diff_line() {
        // Arrange
        let session = session_fixture();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let snapshot = cached_review_comment_snapshot(mixed_review_comment_snapshot());
        let diff = concat!(
            "diff --git a/src/main.rs b/src/main.rs\n",
            "@@ -1,2 +1,2 @@\n",
            " unchanged\n",
            "+new content\n"
        );
        let mut diff_page = DiffPage::new(DiffPageInput {
            diff,
            diff_layout_cache: test_diff_layout_cache(),
            file_explorer_selected_index: 0,
            markdown_render_cache: &markdown_render_cache,
            review_comment_snapshot: Some(&snapshot),
            right_panel: DiffRightPanel::Diff,
            scroll_offset: 0,
            session: &session,
        });
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
        assert!(text.contains("1 comments · new · unresolved"));
        assert!(text.contains("alice"));
        assert!(text.contains("Please handle this edge case."));
        assert!(!text.contains("bob"));
        assert!(!text.contains("Resolved and should stay hidden."));
    }

    #[test]
    fn test_comments_panel_hides_resolved_threads_from_counts_and_body() {
        // Arrange
        let session = session_with_review_request();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let snapshot = cached_review_comment_snapshot(mixed_review_comment_snapshot());
        let diff_page = DiffPage::new(DiffPageInput {
            diff: "",
            diff_layout_cache: test_diff_layout_cache(),
            file_explorer_selected_index: 0,
            markdown_render_cache: &markdown_render_cache,
            review_comment_snapshot: Some(&snapshot),
            right_panel: DiffRightPanel::Comments,
            scroll_offset: 0,
            session: &session,
        });

        // Act
        let lines = diff_page.build_comments_lines(120);
        let text = line_text(&lines);

        // Assert
        assert!(text.contains("1 comments in 1 threads"));
        assert!(text.contains("alice"));
        assert!(text.contains("Please handle this edge case."));
        assert!(!text.contains("bob"));
        assert!(!text.contains("Resolved and should stay hidden."));
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
        let mut diff_page = new_diff_page(&session, &diff, 12, 0, &markdown_render_cache);
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
        let mut diff_page = new_diff_page(&session, &diff, u16::MAX, 0, &markdown_render_cache);
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
