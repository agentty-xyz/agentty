use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::Hasher;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use ag_tui_text::text_util::{self, inline_text};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use rustc_hash::FxHasher;

use crate::domain::session::Session;
use crate::presentation::app_mode::{
    DiffFocus, DiffPreview, DiffPreviewUnavailableReason, DiffReviewComments, DiffSidebarFocus,
};
use crate::presentation::{help_action, review_comment as review_comment_selection};
use crate::ui::component::file_explorer::FileExplorer;
use crate::ui::component::vertical_scrollbar::VerticalScrollbar;
#[cfg(test)]
use crate::ui::component::vertical_scrollbar::{SCROLLBAR_THUMB_SYMBOL, SCROLLBAR_TRACK_SYMBOL};
use crate::ui::diff_util::{
    DiffLine, DiffLineKind, FileTreeItem, diff_header_new_path, parse_diff_lines,
};
use crate::ui::page::review_comment;
use crate::ui::{Component, Page, diff_util, markdown, style};

const WRAPPED_CHUNK_START_INDEX: usize = 0;
const DIFF_CONTENT_CACHE_ENTRY_LIMIT: usize = 8;
const DIFF_LAYOUT_CACHE_ENTRY_LIMIT: usize = 16;
const FILE_LIST_CHANGE_TOTAL_SPAN_COUNT: usize = 4;

/// Compact identity for one raw diff string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiffContentCacheKey {
    content_hash: u64,
    content_len: usize,
    style_version: u64,
}

/// Cache key for one fully assembled diff-panel layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiffLayoutCacheKey {
    diff_area_height: u16,
    diff_area_width: u16,
    diff_content: DiffContentCacheKey,
    reserve_scrollbar_width: bool,
    selected_index: usize,
    style_version: u64,
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
    file_line_ranges: Arc<HashMap<String, Vec<Range<usize>>>>,
    file_list_lines: Arc<[Line<'static>]>,
    key: DiffContentCacheKey,
    parsed_lines: Arc<[OwnedDiffLine]>,
    tree_items: Arc<[FileTreeItem]>,
}

/// Added/removed line totals accumulated while building cached summaries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DiffChangeTotals {
    added_lines: usize,
    removed_lines: usize,
}

impl DiffChangeTotals {
    /// Returns the change represented by one parsed diff line.
    fn from_line_kind(kind: DiffLineKind) -> Option<Self> {
        match kind {
            DiffLineKind::Addition => Some(Self {
                added_lines: 1,
                removed_lines: 0,
            }),
            DiffLineKind::Deletion => Some(Self {
                added_lines: 0,
                removed_lines: 1,
            }),
            DiffLineKind::Context | DiffLineKind::FileHeader | DiffLineKind::HunkHeader => None,
        }
    }

    /// Adds another set of totals to this accumulator.
    fn add(&mut self, totals: Self) {
        self.added_lines = self.added_lines.saturating_add(totals.added_lines);
        self.removed_lines = self.removed_lines.saturating_add(totals.removed_lines);
    }
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

    /// Returns cached diff body rows for `path` without walking unrelated
    /// files.
    pub(crate) fn file_lines(&self, path: &str) -> Vec<DiffLine<'_>> {
        let Some(ranges) = self.file_line_ranges.get(path) else {
            return Vec::new();
        };

        ranges
            .iter()
            .flat_map(|range| self.parsed_lines[range.clone()].iter())
            .filter(|line| {
                matches!(
                    line.kind,
                    DiffLineKind::Addition | DiffLineKind::Deletion | DiffLineKind::Context
                )
            })
            .map(OwnedDiffLine::borrowed)
            .collect()
    }

    /// Returns the selected repository-relative markdown file path.
    pub(crate) fn selected_markdown_path(&self, selected_index: usize) -> Option<&str> {
        let FileTreeItem::File(path) = self.tree_items.get(selected_index)? else {
            return None;
        };
        let extension = Path::new(path).extension()?.to_str()?;
        if !extension.eq_ignore_ascii_case("md") {
            return None;
        }

        Some(path)
    }

    /// Returns parsed lines for the active file-tree selection.
    fn selected_lines(&self, selected_index: usize) -> Vec<DiffLine<'_>> {
        let parsed_lines = self.borrowed_lines();
        let Some(selected_item) = self.tree_items.get(selected_index) else {
            return parsed_lines;
        };

        diff_util::filter_diff_lines(&parsed_lines, selected_item)
    }

    /// Returns whether the active file-tree row identifies one file.
    pub(crate) fn selected_item_is_file(&self, selected_index: usize) -> bool {
        matches!(
            self.tree_items.get(selected_index),
            Some(FileTreeItem::File(_))
        )
    }

    /// Returns the complete cached diff snapshot as borrowed lines.
    fn borrowed_lines(&self) -> Vec<DiffLine<'_>> {
        self.parsed_lines
            .iter()
            .map(OwnedDiffLine::borrowed)
            .collect()
    }

    /// Builds cached added/removed totals for each selectable tree item in one
    /// pass over parsed lines.
    fn change_totals_by_tree_item(
        parsed_lines: &[DiffLine<'_>],
        tree_items: &[FileTreeItem],
    ) -> Vec<DiffChangeTotals> {
        let mut current_path = None;
        let mut file_totals: HashMap<String, DiffChangeTotals> = HashMap::new();

        for diff_line in parsed_lines {
            if diff_line.kind == DiffLineKind::FileHeader
                && diff_line.content.starts_with("diff --git")
            {
                current_path = diff_header_new_path(diff_line.content);
                if let Some(path) = &current_path {
                    file_totals.entry(path.clone()).or_default();
                }

                continue;
            }

            let Some(line_totals) = DiffChangeTotals::from_line_kind(diff_line.kind) else {
                continue;
            };

            if let Some(totals) = current_path
                .as_ref()
                .and_then(|path| file_totals.get_mut(path))
            {
                totals.add(line_totals);
            }
        }

        let folder_totals = Self::folder_totals(&file_totals);
        tree_items
            .iter()
            .map(|item| Self::tree_item_change_totals(item, &file_totals, &folder_totals))
            .collect()
    }

    /// Appends color-coded added/removed totals to every selectable file-tree
    /// line.
    fn append_file_list_change_totals(
        file_list_lines: &mut [Line<'static>],
        change_totals: &[DiffChangeTotals],
    ) {
        for (line, totals) in file_list_lines.iter_mut().zip(change_totals) {
            line.spans.push(Span::raw(" "));
            line.spans.push(Span::styled(
                format!("+{}", totals.added_lines),
                Style::default().fg(style::palette::success()),
            ));
            line.spans.push(Span::styled(
                "/",
                Style::default().fg(style::palette::text_muted()),
            ));
            line.spans.push(Span::styled(
                format!("-{}", totals.removed_lines),
                Style::default().fg(style::palette::danger()),
            ));
        }
    }

    /// Indexes each old and new file path to its parsed-line ranges.
    fn file_line_ranges(parsed_lines: &[DiffLine<'_>]) -> HashMap<String, Vec<Range<usize>>> {
        let mut file_line_ranges = HashMap::new();
        let mut current_paths = None;
        let mut current_start_index = 0;

        for (line_index, line) in parsed_lines.iter().enumerate() {
            if line.kind != DiffLineKind::FileHeader || !line.content.starts_with("diff --git") {
                continue;
            }

            if let Some(paths) = current_paths.take() {
                Self::store_file_line_range(
                    &mut file_line_ranges,
                    paths,
                    current_start_index..line_index,
                );
            }
            current_paths = diff_util::diff_header_paths(line.content);
            current_start_index = line_index.saturating_add(1);
        }

        if let Some(paths) = current_paths {
            Self::store_file_line_range(
                &mut file_line_ranges,
                paths,
                current_start_index..parsed_lines.len(),
            );
        }

        file_line_ranges
    }

    /// Adds one file block under both rename-aware paths without duplication.
    fn store_file_line_range(
        file_line_ranges: &mut HashMap<String, Vec<Range<usize>>>,
        (old_path, new_path): (String, String),
        range: Range<usize>,
    ) {
        file_line_ranges
            .entry(old_path.clone())
            .or_default()
            .push(range.clone());
        if new_path != old_path {
            file_line_ranges.entry(new_path).or_default().push(range);
        }
    }

    /// Aggregates file-level change totals into folder-prefix totals.
    fn folder_totals(
        file_totals: &HashMap<String, DiffChangeTotals>,
    ) -> HashMap<String, DiffChangeTotals> {
        let mut folder_totals: HashMap<String, DiffChangeTotals> = HashMap::new();

        for (path, totals) in file_totals {
            for folder_prefix in Self::folder_prefixes(path) {
                folder_totals.entry(folder_prefix).or_default().add(*totals);
            }
        }

        folder_totals
    }

    /// Returns every folder prefix for a repository-relative path.
    fn folder_prefixes(path: &str) -> Vec<String> {
        path.char_indices()
            .filter_map(|(char_index, character)| {
                if character == '/' {
                    return Some(path[..=char_index].to_string());
                }

                None
            })
            .collect()
    }

    /// Looks up cached change totals for one file-tree item.
    fn tree_item_change_totals(
        item: &FileTreeItem,
        file_totals: &HashMap<String, DiffChangeTotals>,
        folder_totals: &HashMap<String, DiffChangeTotals>,
    ) -> DiffChangeTotals {
        match item {
            FileTreeItem::File(path) => file_totals.get(path.as_str()).copied().unwrap_or_default(),
            FileTreeItem::Folder(path) => folder_totals.get(path).copied().unwrap_or_default(),
        }
    }
}

/// Cached fully assembled diff lines for one render-affecting key.
#[derive(Clone)]
struct DiffCachedLayout {
    changed_line_ranges: Arc<[Range<usize>]>,
    line_count: usize,
    lines: Arc<[Line<'static>]>,
    render_layout: diff_util::DiffRenderLayout,
}

/// Borrowed inputs used to derive or look up one cached diff layout.
#[derive(Clone, Copy)]
struct DiffLayoutRequest<'a> {
    content: &'a DiffContentSnapshot,
    diff_area: Rect,
    reserve_scrollbar_width: bool,
    selected_index: usize,
}

/// Final diff layout selected for the current panel and scrollbar state.
#[derive(Clone)]
pub(crate) struct DiffResolvedLayout {
    pub(crate) changed_line_ranges: Arc<[Range<usize>]>,
    pub(crate) line_count: usize,
    pub(crate) lines: Arc<[Line<'static>]>,
    pub(crate) render_layout: diff_util::DiffRenderLayout,
    pub(crate) show_scrollbar: bool,
}

impl DiffResolvedLayout {
    /// Returns the number of rendered addition and deletion source lines.
    pub(crate) fn changed_line_count(&self) -> usize {
        self.changed_line_ranges.len()
    }

    /// Returns the scroll offset that keeps one changed source line visible.
    pub(crate) fn changed_line_scroll_offset(
        &self,
        selected_diff_line_index: usize,
        current_scroll_offset: u16,
    ) -> Option<u16> {
        let selected_range = self.changed_line_ranges.get(selected_diff_line_index)?;
        let viewport_height = usize::from(self.render_layout.viewport_height);
        if viewport_height == 0 {
            return Some(0);
        }

        let current_start = usize::from(current_scroll_offset);
        let current_end = current_start.saturating_add(viewport_height);
        let next_scroll_offset = if selected_range.start < current_start {
            selected_range.start
        } else if selected_range.end > current_end {
            selected_range.end.saturating_sub(viewport_height)
        } else {
            current_start
        };
        let next_scroll_offset = u16::try_from(next_scroll_offset).unwrap_or(u16::MAX);

        Some(diff_util::clamp_diff_scroll_offset(
            next_scroll_offset,
            self.line_count,
            self.render_layout.viewport_height,
        ))
    }
}

/// Fully assembled diff rows and the rendered range owned by each changed
/// source line.
struct DiffBuiltLines {
    changed_line_ranges: Vec<Range<usize>>,
    lines: Vec<Line<'static>>,
}

/// Final markdown-preview rows selected for the current panel width.
struct DiffPreviewLayout {
    lines: Arc<[Line<'static>]>,
    show_scrollbar: bool,
    viewport_height: u16,
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
/// file-tree metadata or per-path line ranges on every frame. Its key includes
/// the raw diff's hash and byte length plus the active style version, so
/// replacing the diff or theme invalidates the styled snapshot. The
/// rendered-layout layer sits above styled diff assembly so
/// scroll metrics and frame painting
/// reuse the same rows until diff content, selection, panel width/height,
/// scrollbar gutter state, or the active style version changes. Both LRU
/// layers evict their oldest entries at their fixed limits.
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
        let (mut file_list_lines, tree_items) = FileExplorer::file_tree(&parsed_lines);
        let change_totals =
            DiffContentSnapshot::change_totals_by_tree_item(&parsed_lines, &tree_items);
        DiffContentSnapshot::append_file_list_change_totals(&mut file_list_lines, &change_totals);
        let file_line_ranges = DiffContentSnapshot::file_line_ranges(&parsed_lines);
        let snapshot = DiffContentSnapshot {
            file_line_ranges: Arc::new(file_line_ranges),
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
    ) -> DiffResolvedLayout {
        let layout_without_scrollbar = self.layout(DiffLayoutRequest {
            content,
            diff_area,
            reserve_scrollbar_width: false,
            selected_index,
        });
        let show_scrollbar = diff_util::diff_has_scrollable_overflow(
            layout_without_scrollbar.line_count,
            layout_without_scrollbar.render_layout.viewport_height,
        );
        if !show_scrollbar {
            return DiffResolvedLayout {
                changed_line_ranges: layout_without_scrollbar.changed_line_ranges,
                line_count: layout_without_scrollbar.line_count,
                lines: layout_without_scrollbar.lines,
                render_layout: layout_without_scrollbar.render_layout,
                show_scrollbar: false,
            };
        }

        let layout_with_scrollbar = self.layout(DiffLayoutRequest {
            content,
            diff_area,
            reserve_scrollbar_width: true,
            selected_index,
        });
        let show_scrollbar = diff_util::diff_has_scrollable_overflow(
            layout_with_scrollbar.line_count,
            layout_with_scrollbar.render_layout.viewport_height,
        );

        DiffResolvedLayout {
            changed_line_ranges: layout_with_scrollbar.changed_line_ranges,
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
            reserve_scrollbar_width,
            selected_index,
        } = request;
        let key = DiffLayoutCacheKey {
            diff_area_height: diff_area.height,
            diff_area_width: diff_area.width,
            diff_content: content.key,
            reserve_scrollbar_width,
            selected_index,
            style_version: style::active_theme_cache_version(),
        };
        if let Some(layout) = self.cached_layout(&key) {
            return layout;
        }

        let selected_lines = content.selected_lines(selected_index);
        let render_layout =
            diff_util::diff_render_layout(&selected_lines, diff_area, reserve_scrollbar_width);
        let built_lines = DiffPage::build_diff_lines(&selected_lines, render_layout);
        let layout = DiffCachedLayout {
            changed_line_ranges: Arc::from(built_lines.changed_line_ranges),
            line_count: built_lines.lines.len(),
            lines: Arc::from(built_lines.lines),
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

    /// Returns a compact key for the raw diff string and active UI theme.
    fn content_cache_key(diff: &str) -> DiffContentCacheKey {
        let mut hasher = FxHasher::default();
        hasher.write(diff.as_bytes());

        DiffContentCacheKey {
            content_hash: hasher.finish(),
            content_len: diff.len(),
            style_version: style::active_theme_cache_version(),
        }
    }
}

/// Renders the current session's git diff in a scrollable page.
pub struct DiffPage<'a> {
    /// Raw unified diff currently shown by the page.
    pub diff: &'a str,
    /// Shared cache for parsed diff content and rendered layouts.
    pub diff_layout_cache: &'a DiffLayoutCache,
    /// Selected file-tree row in the left panel.
    pub file_explorer_selected_index: usize,
    /// Panel currently receiving changed-file navigation input.
    pub focus: DiffFocus,
    /// Shared cache for rendered markdown preview rows.
    pub markdown_render_cache: &'a markdown::MarkdownRenderCache,
    /// Rendered-markdown preview state for the selected file.
    pub preview: &'a DiffPreview,
    /// Optional linked review-request comments shown below changed files.
    pub review_comments: Option<&'a DiffReviewComments>,
    /// Vertical scroll offset inside the diff panel.
    pub scroll_offset: u16,
    /// Addition or deletion selected in the right-hand diff panel.
    pub selected_diff_line_index: usize,
    /// Session whose diff is being rendered.
    pub session: &'a Session,
    /// Sidebar section currently controlling the right pane.
    pub sidebar_focus: DiffSidebarFocus,
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
    /// Panel currently receiving changed-file navigation input.
    pub focus: DiffFocus,
    /// Shared cache for rendered markdown preview rows.
    pub markdown_render_cache: &'a markdown::MarkdownRenderCache,
    /// Rendered-markdown preview state for the selected file.
    pub preview: &'a DiffPreview,
    /// Optional linked review-request comments shown below changed files.
    pub review_comments: Option<&'a DiffReviewComments>,
    /// Vertical scroll offset inside the diff panel.
    pub scroll_offset: u16,
    /// Addition or deletion selected in the right-hand diff panel.
    pub selected_diff_line_index: usize,
    /// Session whose diff is being rendered.
    pub session: &'a Session,
    /// Sidebar section currently controlling the right pane.
    pub sidebar_focus: DiffSidebarFocus,
}

impl<'a> DiffPage<'a> {
    /// Creates a diff page for the given session and scroll position.
    pub fn new(input: DiffPageInput<'a>) -> Self {
        let DiffPageInput {
            diff,
            diff_layout_cache,
            file_explorer_selected_index,
            focus,
            markdown_render_cache,
            preview,
            review_comments,
            scroll_offset,
            selected_diff_line_index,
            session,
            sidebar_focus,
        } = input;

        Self {
            diff,
            diff_layout_cache,
            file_explorer_selected_index,
            focus,
            markdown_render_cache,
            preview,
            review_comments,
            scroll_offset,
            selected_diff_line_index,
            session,
            sidebar_focus,
        }
    }

    /// Renders the right-side diff panel with line-number gutters and
    /// aggregate change totals prefixed in the title.
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
        );

        let scroll_offset = diff_util::clamp_diff_scroll_offset(
            self.scroll_offset,
            layout.line_count,
            layout.render_layout.viewport_height,
        );
        let selected_range = (self.focus == DiffFocus::Content)
            .then(|| {
                layout
                    .changed_line_ranges
                    .get(self.selected_diff_line_index)
            })
            .flatten();
        let paint_lines = Self::borrowed_visible_lines(
            &layout.lines,
            scroll_offset,
            layout.render_layout.viewport_height,
            selected_range,
        );

        let paragraph = Paragraph::new(paint_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(self.content_border_style()),
        );

        f.render_widget(paragraph, area);

        if layout.show_scrollbar {
            let scrollbar_area =
                diff_util::diff_scrollbar_area(area, layout.render_layout.viewport_height);

            VerticalScrollbar::new(scroll_offset, layout.line_count).render(f, scrollbar_area);
        }
    }

    /// Renders ready markdown content or a preview availability notice.
    fn render_preview_content(&self, frame: &mut Frame, area: Rect, path: &str) {
        let title = Line::from(Span::styled(
            format!(" Preview — {} ", inline_text(path)),
            Style::default().fg(style::palette::warning()),
        ));
        match self.preview {
            DiffPreview::Ready { content, .. } => {
                let layout = diff_preview_layout(content, area, self.markdown_render_cache);
                let scroll_offset = diff_util::clamp_diff_scroll_offset(
                    self.scroll_offset,
                    layout.lines.len(),
                    layout.viewport_height,
                );
                let paint_lines = Self::borrowed_visible_lines(
                    &layout.lines,
                    scroll_offset,
                    layout.viewport_height,
                    None,
                );
                let paragraph = Paragraph::new(paint_lines).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(title)
                        .border_style(self.content_border_style()),
                );
                frame.render_widget(paragraph, area);

                if layout.show_scrollbar {
                    let scrollbar_area =
                        diff_util::diff_scrollbar_area(area, layout.viewport_height);
                    VerticalScrollbar::new(scroll_offset, layout.lines.len())
                        .render(frame, scrollbar_area);
                }
            }
            DiffPreview::Loading { .. } => {
                render_preview_notice(
                    frame,
                    area,
                    title,
                    " Loading preview… ",
                    self.content_border_style(),
                );
            }
            DiffPreview::Unavailable { reason, .. } => {
                render_preview_notice(
                    frame,
                    area,
                    title,
                    preview_unavailable_message(reason),
                    self.content_border_style(),
                );
            }
            DiffPreview::Off { .. } | DiffPreview::Unsupported { .. } => {}
        }
    }

    /// Builds short-lived paint rows for the visible viewport slice, borrowing
    /// span content from cached static diff rows instead of cloning the whole
    /// diff on every scroll repaint.
    fn borrowed_visible_lines<'line>(
        lines: &'line [Line<'static>],
        scroll_offset: u16,
        viewport_height: u16,
        selected_range: Option<&Range<usize>>,
    ) -> Vec<Line<'line>> {
        let start_index = usize::from(scroll_offset).min(lines.len());
        let end_index = start_index
            .saturating_add(usize::from(viewport_height))
            .min(lines.len());

        lines[start_index..end_index]
            .iter()
            .enumerate()
            .map(|(visible_index, line)| {
                let mut paint_line = text_util::borrowed_paint_line(line);
                let rendered_index = start_index.saturating_add(visible_index);
                if selected_range.is_some_and(|range| range.contains(&rendered_index)) {
                    paint_line.style = paint_line.style.add_modifier(Modifier::REVERSED);
                    for span in &mut paint_line.spans {
                        span.style = span.style.add_modifier(Modifier::REVERSED);
                    }
                }

                paint_line
            })
            .collect()
    }

    /// Returns accent chrome while the right-hand changed-line cursor owns
    /// focus.
    fn content_border_style(&self) -> Style {
        if self.focus == DiffFocus::Content {
            return Style::default()
                .fg(style::palette::accent())
                .add_modifier(Modifier::BOLD);
        }

        style::border_style()
    }

    /// Builds wrapped diff lines for the diff panel, optionally reserving one
    /// column for the scrollbar thumb.
    fn build_diff_lines(
        parsed: &[DiffLine<'_>],
        layout: diff_util::DiffRenderLayout,
    ) -> DiffBuiltLines {
        let gutter_style = diff_util::body_diff_line_gutter_style();
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(parsed.len());
        let mut changed_line_ranges = Vec::new();

        for diff_line in parsed {
            let rendered_start_index = lines.len();
            if Self::append_special_diff_line(&mut lines, diff_line) {
                continue;
            }

            Self::append_body_diff_line(&mut lines, diff_line, layout, gutter_style);
            let is_changed_line = diff_line.kind == DiffLineKind::Addition
                || diff_line.kind == DiffLineKind::Deletion;
            if is_changed_line {
                changed_line_ranges.push(rendered_start_index..lines.len());
            }
        }

        if lines.is_empty() {
            lines.push(Line::from(" No changes found. "));
        }

        DiffBuiltLines {
            changed_line_ranges,
            lines,
        }
    }

    /// Appends file and hunk headers, returning whether the line was consumed.
    fn append_special_diff_line(lines: &mut Vec<Line<'static>>, diff_line: &DiffLine<'_>) -> bool {
        match diff_line.kind {
            DiffLineKind::FileHeader => {
                Self::append_file_header_diff_line(lines, diff_line);

                true
            }
            DiffLineKind::HunkHeader => {
                lines.push(Line::from(Span::styled(
                    diff_line.content.to_string(),
                    Style::default().fg(style::palette::accent()),
                )));

                true
            }
            DiffLineKind::Addition | DiffLineKind::Deletion | DiffLineKind::Context => false,
        }
    }

    /// Appends one file-header diff line.
    fn append_file_header_diff_line(lines: &mut Vec<Line<'static>>, diff_line: &DiffLine<'_>) {
        if diff_line.content.starts_with("diff ") && !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            diff_line.content.to_string(),
            Style::default().fg(style::palette::warning()),
        )));
    }

    /// Appends one addition, deletion, or context line with wrapped content.
    fn append_body_diff_line(
        lines: &mut Vec<Line<'static>>,
        diff_line: &DiffLine<'_>,
        layout: diff_util::DiffRenderLayout,
        gutter_style: Style,
    ) {
        let (sign, content_style) = diff_util::body_diff_line_style(diff_line.kind);
        let gutter_text = diff_util::body_diff_line_gutter(diff_line, layout.gutter_width);
        let content_available = layout.content_width.saturating_sub(layout.prefix_width);
        let chunks = diff_util::wrap_diff_content(diff_line.content, content_available);

        for (index, chunk) in chunks.iter().enumerate() {
            if index == WRAPPED_CHUNK_START_INDEX {
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
    }
}

/// Returns the max valid scroll offset for the selected diff panel.
pub(crate) fn diff_view_max_scroll_offset(
    diff: &str,
    selected_index: usize,
    terminal_area: Rect,
    diff_layout_cache: &DiffLayoutCache,
    markdown_render_cache: &markdown::MarkdownRenderCache,
    preview: &DiffPreview,
) -> u16 {
    let diff_area = diff_util::diff_page_areas(terminal_area).diff_area;
    let content = diff_layout_cache.content(diff);
    if preview_path_for_selection(preview, &content, selected_index).is_some() {
        return match preview {
            DiffPreview::Ready {
                content: markdown_content,
                ..
            } => {
                let layout =
                    diff_preview_layout(markdown_content, diff_area, markdown_render_cache);

                diff_util::clamp_diff_scroll_offset(
                    u16::MAX,
                    layout.lines.len(),
                    layout.viewport_height,
                )
            }
            _ => 0,
        };
    }
    let layout = diff_layout_cache.resolved_layout(&content, selected_index, diff_area);
    if layout.render_layout.viewport_height == 0 {
        return 0;
    }

    diff_util::clamp_diff_scroll_offset(
        u16::MAX,
        layout.line_count,
        layout.render_layout.viewport_height,
    )
}

/// Returns cached layout metadata for changed-line navigation in one file.
pub(crate) fn diff_changed_line_layout(
    diff: &str,
    selected_file_index: usize,
    terminal_area: Rect,
    diff_layout_cache: &DiffLayoutCache,
) -> DiffResolvedLayout {
    let diff_area = diff_util::diff_page_areas(terminal_area).diff_area;
    let content = diff_layout_cache.content(diff);

    diff_layout_cache.resolved_layout(&content, selected_file_index, diff_area)
}

impl Page for DiffPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let areas = diff_util::diff_page_areas(area);
        let content = self.diff_layout_cache.content(self.diff);
        let sidebar_areas =
            diff_util::diff_sidebar_areas(areas.file_list_area, self.review_comments.is_some());

        FileExplorer::from_cached_lines(
            content.file_list_lines(),
            FILE_LIST_CHANGE_TOTAL_SPAN_COUNT,
        )
        .selected_index(self.file_explorer_selected_index)
        .focused(self.sidebar_focus == DiffSidebarFocus::Files && self.focus == DiffFocus::Files)
        .render(f, sidebar_areas.file_list_area);

        let review_comment_page = self.review_comments.map(|review_comments| {
            let rows = review_comments
                .comment_snapshot
                .as_ref()
                .map(review_comment_selection::grouped_review_comment_rows)
                .unwrap_or_default();
            let page =
                review_comment::ReviewCommentPage::new(review_comment::ReviewCommentPageInput {
                    comment_actions: &review_comments.comment_actions,
                    comment_error: review_comments.comment_error.as_deref(),
                    comment_snapshot: review_comments.comment_snapshot.as_ref(),
                    diff: self.diff,
                    is_loading_comments: review_comments.is_loading_comments,
                    render_caches: review_comment::ReviewCommentRenderCaches {
                        diff_layout: self.diff_layout_cache,
                        markdown: self.markdown_render_cache,
                    },
                    scroll_offset: self.scroll_offset,
                    selected_comment_index: review_comments.selected_comment_index,
                    session: self.session,
                });
            page.render_comment_list(
                f,
                sidebar_areas.comment_list_area,
                &rows,
                self.sidebar_focus == DiffSidebarFocus::Comments,
            );

            (page, rows)
        });

        if let Some((review_comment_page, rows)) = review_comment_page
            && self.sidebar_focus == DiffSidebarFocus::Comments
        {
            review_comment_page.render_comment_detail(f, areas.diff_area, &rows);
        } else if let Some(path) =
            preview_path_for_selection(self.preview, &content, self.file_explorer_selected_index)
        {
            self.render_preview_content(f, areas.diff_area, path);
        } else {
            self.render_diff_content(
                f,
                areas.diff_area,
                &content,
                self.session.stats.added_lines,
                self.session.stats.deleted_lines,
            );
        }

        let (can_mark_selected, can_submit) = if self.sidebar_focus == DiffSidebarFocus::Comments {
            self.review_comments
                .map_or((false, false), |review_comments| {
                    let rows = review_comments
                        .comment_snapshot
                        .as_ref()
                        .map(review_comment_selection::grouped_review_comment_rows)
                        .unwrap_or_default();
                    let can_reply = self.session.allows_review_comment_reply();

                    (
                        can_reply
                            && review_comment::review_comment_selected_is_actionable(
                                &rows,
                                review_comments.selected_comment_index,
                            ),
                        can_reply && !review_comments.comment_actions.is_empty(),
                    )
                })
        } else {
            (false, false)
        };
        let help_message = Paragraph::new(crate::ui::help_format::footer_line(
            &help_action::diff_footer_actions(
                self.review_comments.is_some(),
                self.sidebar_focus,
                self.focus,
                can_mark_selected,
                can_submit,
            ),
        ));
        f.render_widget(help_message, areas.footer_area);
    }
}

/// Returns the preview path when it still matches the active markdown row.
fn preview_path_for_selection<'a>(
    preview: &'a DiffPreview,
    content: &DiffContentSnapshot,
    selected_index: usize,
) -> Option<&'a str> {
    let selected_path = content.selected_markdown_path(selected_index)?;
    let preview_path = preview.path()?;
    if preview_path != selected_path {
        return None;
    }

    Some(preview_path)
}

/// Resolves cached markdown rows with a scrollbar-width second pass.
fn diff_preview_layout(
    content: &str,
    area: Rect,
    markdown_render_cache: &markdown::MarkdownRenderCache,
) -> DiffPreviewLayout {
    let viewport_height = area.height.saturating_sub(2);
    let content_width = usize::from(area.width.saturating_sub(2));
    let lines_without_scrollbar = markdown_render_cache.render(content, content_width);
    let show_scrollbar =
        diff_util::diff_has_scrollable_overflow(lines_without_scrollbar.len(), viewport_height);
    let lines = if show_scrollbar {
        markdown_render_cache.render(content, content_width.saturating_sub(1))
    } else {
        lines_without_scrollbar
    };

    DiffPreviewLayout {
        show_scrollbar: diff_util::diff_has_scrollable_overflow(lines.len(), viewport_height),
        lines,
        viewport_height,
    }
}

/// Renders one bordered preview loading or availability message.
fn render_preview_notice(
    frame: &mut Frame,
    area: Rect,
    title: Line<'static>,
    message: &str,
    border_style: Style,
) {
    let paragraph = Paragraph::new(Line::from(message.to_string())).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(border_style),
    );
    frame.render_widget(paragraph, area);
}

/// Returns the concise notice for one unavailable preview reason.
fn preview_unavailable_message(reason: &DiffPreviewUnavailableReason) -> &str {
    match reason {
        DiffPreviewUnavailableReason::Deleted => " File deleted in this change. ",
        DiffPreviewUnavailableReason::Binary => " Binary file — no preview. ",
        DiffPreviewUnavailableReason::TooLarge => " File too large to preview. ",
        DiffPreviewUnavailableReason::LoadFailed(error) => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::theme::ColorTheme;
    use crate::test_support::SessionFixtureBuilder;
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

    fn new_diff_page<'a>(
        session: &'a Session,
        diff: &'a str,
        scroll_offset: u16,
        file_explorer_selected_index: usize,
    ) -> DiffPage<'a> {
        DiffPage::new(DiffPageInput {
            diff,
            diff_layout_cache: test_diff_layout_cache(),
            file_explorer_selected_index,
            focus: DiffFocus::Files,
            markdown_render_cache: test_markdown_render_cache(),
            preview: test_diff_preview(),
            review_comments: None,
            scroll_offset,
            selected_diff_line_index: 0,
            session,
            sidebar_focus: DiffSidebarFocus::Files,
        })
    }

    fn new_diff_page_with_preview<'a>(
        session: &'a Session,
        diff: &'a str,
        scroll_offset: u16,
        file_explorer_selected_index: usize,
        preview: &'a DiffPreview,
    ) -> DiffPage<'a> {
        DiffPage::new(DiffPageInput {
            diff,
            diff_layout_cache: test_diff_layout_cache(),
            file_explorer_selected_index,
            focus: DiffFocus::Files,
            markdown_render_cache: test_markdown_render_cache(),
            preview,
            review_comments: None,
            scroll_offset,
            selected_diff_line_index: 0,
            session,
            sidebar_focus: DiffSidebarFocus::Files,
        })
    }

    fn test_diff_layout_cache() -> &'static DiffLayoutCache {
        Box::leak(Box::new(DiffLayoutCache::default()))
    }

    fn test_markdown_render_cache() -> &'static markdown::MarkdownRenderCache {
        Box::leak(Box::new(markdown::MarkdownRenderCache::default()))
    }

    fn test_diff_preview() -> &'static DiffPreview {
        Box::leak(Box::new(DiffPreview::default()))
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

    fn modifier_cell_count(buffer: &ratatui::buffer::Buffer, modifier: Modifier) -> usize {
        buffer
            .content()
            .iter()
            .filter(|cell| cell.modifier.contains(modifier))
            .count()
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
            &first_content.file_line_ranges,
            &second_content.file_line_ranges
        ));
        assert!(Arc::ptr_eq(
            &first_content.file_list_lines,
            &second_content.file_list_lines
        ));
    }

    #[test]
    fn test_diff_content_snapshot_rebuilds_styled_file_list_after_theme_change() {
        // Arrange
        let cache = DiffLayoutCache::default();
        let (current_lines, current_success_color, expected_current_success) = {
            let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
            let content = cache.content(SAMPLE_DIFF);
            let lines = content.file_list_lines();
            let success_color = lines[0].spans[2].style.fg;

            (lines, success_color, Some(style::palette::success()))
        };

        // Act
        let (green_lines, green_success_color, expected_green_success) = {
            let _theme_scope = style::scoped_active_theme(ColorTheme::Green);
            let content = cache.content(SAMPLE_DIFF);
            let lines = content.file_list_lines();
            let success_color = lines[0].spans[2].style.fg;

            (lines, success_color, Some(style::palette::success()))
        };

        // Assert
        assert!(!Arc::ptr_eq(&current_lines, &green_lines));
        assert_ne!(current_success_color, green_success_color);
        assert_eq!(current_success_color, expected_current_success);
        assert_eq!(green_success_color, expected_green_success);
    }

    #[test]
    fn test_diff_content_snapshot_indexes_repeated_and_renamed_file_blocks() {
        // Arrange
        let cache = DiffLayoutCache::default();
        let content = cache.content(concat!(
            "diff --git a/src/old.rs b/src/new.rs\n",
            "index 111..222 100644\n",
            "@@ -1 +1 @@\n",
            "-old first\n",
            "+new first\n",
            "diff --git malformed\n",
            "+ignored malformed\n",
            "diff --git a/src/new.rs b/src/new.rs\n",
            "@@ -2 +2 @@\n",
            " unchanged second\n",
        ));

        // Act
        let old_path_lines = content.file_lines("src/old.rs");
        let new_path_lines = content.file_lines("src/new.rs");
        let missing_path_lines = content.file_lines("src/missing.rs");

        // Assert
        assert_eq!(
            old_path_lines
                .iter()
                .map(|line| line.content)
                .collect::<Vec<_>>(),
            vec!["old first", "new first"]
        );
        assert_eq!(
            new_path_lines
                .iter()
                .map(|line| line.content)
                .collect::<Vec<_>>(),
            vec!["old first", "new first", "unchanged second"]
        );
        assert!(missing_path_lines.is_empty());
    }

    #[test]
    fn test_diff_content_snapshot_appends_change_totals_to_each_file_tree_line() {
        // Arrange
        let cache = DiffLayoutCache::default();
        let content = cache.content(concat!(
            "diff --git a/src/main.rs b/src/main.rs\n",
            "@@ -1,2 +1,3 @@\n",
            " unchanged\n",
            "+added main\n",
            "-removed main\n",
            "diff --git a/src/ui/diff.rs b/src/ui/diff.rs\n",
            "@@ -1 +1,2 @@\n",
            "+added nested\n",
            "diff --git a/README.md b/README.md\n",
            "@@ -1 +1,2 @@\n",
            "+added readme\n",
        ));

        // Act
        let lines = content.file_list_lines();
        let line_text = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            line_text,
            [
                "src/ +2/-1",
                "├ ui/ +1/-0",
                "│ └ diff.rs +1/-0",
                "└ main.rs +1/-1",
                "README.md +1/-0",
            ]
        );
        assert_eq!(lines[0].spans[2].style.fg, Some(style::palette::success()));
        assert_eq!(
            lines[0].spans[3].style.fg,
            Some(style::palette::text_muted())
        );
        assert_eq!(lines[0].spans[4].style.fg, Some(style::palette::danger()));
    }

    #[test]
    fn test_diff_content_snapshot_identifies_files() {
        // Arrange
        let cache = DiffLayoutCache::default();
        let content = cache.content(SAMPLE_DIFF);

        // Act
        let folder_is_file = content.selected_item_is_file(0);
        let file_is_file = content.selected_item_is_file(1);

        // Assert
        assert!(!folder_is_file);
        assert!(file_is_file);
    }

    #[test]
    fn test_borrowed_visible_lines_reverses_selected_rendered_range() {
        // Arrange
        let lines = [
            Line::from(Span::raw("first")),
            Line::from(Span::raw("selected")),
            Line::from(Span::raw("last")),
        ];

        // Act
        let paint_lines = DiffPage::borrowed_visible_lines(&lines, 0, 3, Some(&(1..2)));

        // Assert
        assert!(
            !paint_lines[0]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            paint_lines[1]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            paint_lines[1].spans[0]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn test_diff_changed_line_layout_counts_changes_and_keeps_cursor_visible() {
        // Arrange
        let diff = format!(
            "diff --git a/src/main.rs b/src/main.rs\n@@ -0,0 +1,40 @@\n{}",
            (0..40)
                .map(|index| format!("+line {index}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let cache = DiffLayoutCache::default();
        let terminal_area = Rect::new(0, 0, 80, 12);
        let zero_height_area = Rect::new(0, 0, 80, 1);

        // Act
        let layout = diff_changed_line_layout(&diff, 0, terminal_area, &cache);
        let scrolled_down = layout
            .changed_line_scroll_offset(20, 0)
            .expect("selected changed line should have a rendered range");
        let scrolled_up = layout
            .changed_line_scroll_offset(0, scrolled_down)
            .expect("first changed line should have a rendered range");
        let zero_height_layout = diff_changed_line_layout(&diff, 0, zero_height_area, &cache);
        let zero_height_scroll_offset = zero_height_layout
            .changed_line_scroll_offset(0, scrolled_down)
            .expect("first changed line should remain selectable without a viewport");

        // Assert
        assert_eq!(layout.changed_line_count(), 40);
        assert!(scrolled_down > 0);
        assert!(scrolled_up < scrolled_down);
        assert_eq!(zero_height_scroll_offset, 0);
    }

    #[test]
    fn test_content_border_style_uses_accent_only_for_diff_focus() {
        // Arrange
        let session = session_fixture();
        let mut page = new_diff_page(&session, SAMPLE_DIFF, 0, 1);
        let file_border_style = page.content_border_style();

        // Act
        page.focus = DiffFocus::Content;
        let content_border_style = page.content_border_style();

        // Assert
        assert_eq!(file_border_style.fg, Some(style::palette::border()));
        assert_eq!(content_border_style.fg, Some(style::palette::accent()));
        assert!(content_border_style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_selected_markdown_path_accepts_case_insensitive_file_extension_only() {
        // Arrange
        let cache = DiffLayoutCache::default();
        let content = cache.content(concat!(
            "diff --git a/docs/GUIDE.MD b/docs/GUIDE.MD\n+guide\n",
            "diff --git a/src/main.rs b/src/main.rs\n+code\n",
        ));

        // Act
        let folder = content.selected_markdown_path(0);
        let markdown = content.selected_markdown_path(1);
        let rust = content.selected_markdown_path(3);
        let stale = content.selected_markdown_path(usize::MAX);

        // Assert
        assert_eq!(folder, None);
        assert_eq!(markdown, Some("docs/GUIDE.MD"));
        assert_eq!(rust, None);
        assert_eq!(stale, None);
    }

    #[test]
    fn test_selected_markdown_preview_path_decodes_git_quoted_filename() {
        // Arrange
        let cache = DiffLayoutCache::default();
        let content = cache.content(concat!(
            "diff --git \"a/docs/\\346\\227\\245\\346\\234\\254.md\" ",
            "\"b/docs/\\346\\227\\245\\346\\234\\254.md\"\n+preview\n",
        ));
        let preview = DiffPreview::Ready {
            content: "# Preview".to_string(),
            path: "docs/日本.md".to_string(),
            request_id: 1,
        };

        // Act
        let selected_path = content.selected_markdown_path(1);
        let preview_path = preview_path_for_selection(&preview, &content, 1);

        // Assert
        assert_eq!(selected_path, Some("docs/日本.md"));
        assert_eq!(preview_path, Some("docs/日本.md"));
    }

    #[test]
    fn test_disabled_preview_states_do_not_resolve_or_render_preview_content() {
        // Arrange
        let session = session_fixture();
        let cache = DiffLayoutCache::default();
        let content = cache.content(SAMPLE_DIFF);
        let previews = [
            DiffPreview::Off { request_id: 1 },
            DiffPreview::Unsupported { request_id: 2 },
        ];
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        for preview in &previews {
            assert!(preview_path_for_selection(preview, &content, 1).is_none());
            terminal
                .draw(|frame| {
                    new_diff_page_with_preview(&session, SAMPLE_DIFF, 0, 1, preview)
                        .render_preview_content(frame, frame.area(), "README.md");
                })
                .expect("failed to draw disabled preview state");
        }

        // Assert
        assert_eq!(buffer_text(terminal.backend().buffer()).trim(), "");
    }

    #[test]
    fn test_diff_layout_cache_reuses_rendered_layout_rows() {
        // Arrange
        let cache = DiffLayoutCache::default();
        let content = cache.content(SAMPLE_DIFF);
        let area = Rect::new(0, 0, 80, 12);

        // Act
        let first_layout = cache.resolved_layout(&content, 0, area);
        let second_layout = cache.resolved_layout(&content, 0, area);

        // Assert
        assert!(Arc::ptr_eq(&first_layout.lines, &second_layout.lines));
        assert_eq!(first_layout.line_count, second_layout.line_count);
    }

    #[test]
    fn test_render_shows_updated_diff_help_hint() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let mut session = session_fixture();
        session.stats.added_lines = 1;
        session.stats.deleted_lines = 0;
        let diff = "diff --git a/src/main.rs b/src/main.rs\n+added";
        let mut diff_page = new_diff_page(&session, diff, 0, 0);
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
        assert_eq!(text.matches("+1/-0").count(), 2);
        assert!(text.contains("j/k: select file"));
        assert!(text.contains("?: help"));
        assert!(foreground_symbol_cell_count(buffer, "┌") >= 2);
    }

    #[test]
    fn test_render_diff_title_uses_persisted_session_line_totals() {
        // Arrange
        let mut session = session_fixture();
        session.stats.added_lines = 9;
        session.stats.deleted_lines = 4;
        let diff = "diff --git a/src/main.rs b/src/main.rs\n+added";
        let mut diff_page = new_diff_page(&session, diff, 0, 0);
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
        assert_eq!(text.matches("+1/-0").count(), 2);
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
        let diff = concat!(
            "diff --git a/src/main.rs b/src/main.rs\n",
            "@@ -1,2 +1,2 @@\n",
            "-old content\n",
            "+new content\n"
        );
        let mut diff_page = new_diff_page(&session, diff, 0, 0);
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
    fn test_render_highlights_selected_changed_line_in_content_focus() {
        // Arrange
        let session = session_fixture();
        let diff = concat!(
            "diff --git a/src/main.rs b/src/main.rs\n",
            "@@ -1,2 +1,2 @@\n",
            "-old content\n",
            "+new content\n"
        );
        let mut diff_page = new_diff_page(&session, diff, 0, 1);
        diff_page.focus = DiffFocus::Content;
        diff_page.selected_diff_line_index = 1;
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut diff_page, frame, area);
            })
            .expect("failed to draw focused diff page");

        // Assert
        let buffer = terminal.backend().buffer();
        assert!(modifier_cell_count(buffer, Modifier::REVERSED) > 0);
        assert!(buffer.content().iter().any(|cell| {
            cell.symbol() == "┌"
                && cell.fg == style::palette::accent()
                && cell.modifier.contains(Modifier::BOLD)
        }));
    }

    #[test]
    fn test_render_shows_scrollbar_for_overflowing_diff() {
        // Arrange
        let session = session_fixture();
        let diff = (0..80)
            .map(|index| format!("+line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut diff_page = new_diff_page(&session, &diff, 12, 0);
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
    fn test_render_ready_preview_uses_shared_markdown_and_mermaid_renderer() {
        // Arrange
        let session = session_fixture();
        let diff = "diff --git a/README.md b/README.md\n+preview";
        let preview = DiffPreview::Ready {
            content: concat!(
                "# Preview Title\n\n",
                "| Name | Value |\n| --- | --- |\n| mode | ready |\n\n",
                "```mermaid\ngraph TD\nA[Input] --> B[Rendered]\n```\n",
            )
            .to_string(),
            path: "README.md".to_string(),
            request_id: 1,
        };
        let mut diff_page = new_diff_page_with_preview(&session, diff, 0, 0, &preview);
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut diff_page, frame, area);
            })
            .expect("failed to draw markdown preview");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Preview — README.md"));
        assert!(text.contains("Preview Title"));
        assert!(text.contains("mode"));
        assert!(text.contains("Input"));
        assert!(text.contains("Rendered"));
        assert!(!text.contains("+preview"));
    }

    #[test]
    fn test_render_preview_loading_and_unavailable_notices() {
        // Arrange
        let session = session_fixture();
        let diff = "diff --git a/README.md b/README.md\n+preview";
        let previews = [
            (
                DiffPreview::Loading {
                    path: "README.md".to_string(),
                    request_id: 1,
                },
                "Loading preview…",
            ),
            (
                DiffPreview::Unavailable {
                    path: "README.md".to_string(),
                    reason: DiffPreviewUnavailableReason::Deleted,
                    request_id: 2,
                },
                "File deleted in this change.",
            ),
            (
                DiffPreview::Unavailable {
                    path: "README.md".to_string(),
                    reason: DiffPreviewUnavailableReason::Binary,
                    request_id: 3,
                },
                "Binary file — no preview.",
            ),
            (
                DiffPreview::Unavailable {
                    path: "README.md".to_string(),
                    reason: DiffPreviewUnavailableReason::TooLarge,
                    request_id: 4,
                },
                "File too large to preview.",
            ),
            (
                DiffPreview::Unavailable {
                    path: "README.md".to_string(),
                    reason: DiffPreviewUnavailableReason::LoadFailed(
                        "Preview read failed".to_string(),
                    ),
                    request_id: 5,
                },
                "Preview read failed",
            ),
        ];

        // Act
        let rendered_text = previews
            .iter()
            .map(|(preview, _)| {
                let mut diff_page = new_diff_page_with_preview(&session, diff, 0, 0, preview);
                let backend = ratatui::backend::TestBackend::new(100, 16);
                let mut terminal =
                    ratatui::Terminal::new(backend).expect("failed to create terminal");
                terminal
                    .draw(|frame| {
                        let area = frame.area();
                        Page::render(&mut diff_page, frame, area);
                    })
                    .expect("failed to draw preview notice");

                buffer_text(terminal.backend().buffer())
            })
            .collect::<Vec<_>>();

        // Assert
        for ((_, expected), text) in previews.iter().zip(rendered_text) {
            assert!(text.contains(expected));
            assert!(text.contains("Preview — README.md"));
        }
    }

    #[test]
    fn test_render_preview_falls_back_when_path_no_longer_matches_selection() {
        // Arrange
        let session = session_fixture();
        let diff = "diff --git a/README.md b/README.md\n+current diff";
        let preview = DiffPreview::Ready {
            content: "# Stale preview".to_string(),
            path: "OTHER.md".to_string(),
            request_id: 1,
        };
        let mut diff_page = new_diff_page_with_preview(&session, diff, 0, 0, &preview);
        let backend = ratatui::backend::TestBackend::new(100, 16);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut diff_page, frame, area);
            })
            .expect("failed to draw diff fallback");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("current diff"));
        assert!(!text.contains("Stale preview"));
    }

    #[test]
    fn test_render_preview_scrollbar_and_max_scroll_share_layout() {
        // Arrange
        let session = session_fixture();
        let diff = "diff --git a/README.md b/README.md\n+preview";
        let markdown_content = (0..80)
            .map(|index| format!("- preview line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let preview = DiffPreview::Ready {
            content: markdown_content,
            path: "README.md".to_string(),
            request_id: 1,
        };
        let diff_layout_cache = DiffLayoutCache::default();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let mut diff_page = DiffPage::new(DiffPageInput {
            diff,
            diff_layout_cache: &diff_layout_cache,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            markdown_render_cache: &markdown_render_cache,
            preview: &preview,
            review_comments: None,
            scroll_offset: 12,
            selected_diff_line_index: 0,
            session: &session,
            sidebar_focus: DiffSidebarFocus::Files,
        });
        let terminal_area = Rect::new(0, 0, 80, 12);
        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        let max_scroll_offset = diff_view_max_scroll_offset(
            diff,
            0,
            terminal_area,
            &diff_layout_cache,
            &markdown_render_cache,
            &preview,
        );
        terminal
            .draw(|frame| Page::render(&mut diff_page, frame, terminal_area))
            .expect("failed to draw scrollable preview");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(max_scroll_offset > 0);
        assert!(text.contains(SCROLLBAR_TRACK_SYMBOL));
        assert!(text.contains(SCROLLBAR_THUMB_SYMBOL));
    }

    #[test]
    fn test_preview_notice_has_zero_max_scroll_offset() {
        // Arrange
        let diff = "diff --git a/README.md b/README.md\n+preview";
        let diff_layout_cache = DiffLayoutCache::default();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let preview = DiffPreview::Loading {
            path: "README.md".to_string(),
            request_id: 1,
        };

        // Act
        let max_scroll_offset = diff_view_max_scroll_offset(
            diff,
            0,
            Rect::new(0, 0, 80, 12),
            &diff_layout_cache,
            &markdown_render_cache,
            &preview,
        );

        // Assert
        assert_eq!(max_scroll_offset, 0);
    }

    #[test]
    fn test_render_clamps_overscroll_to_last_visible_diff_lines() {
        // Arrange
        let session = session_fixture();
        let diff = (0..40)
            .map(|index| format!("+line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut diff_page = new_diff_page(&session, &diff, u16::MAX, 0);
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
