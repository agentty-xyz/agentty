use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::Hasher;
use std::sync::Arc;

use ag_tui_text::text_util::{self, inline_text};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use rustc_hash::FxHasher;

use crate::domain::session::Session;
use crate::presentation::help_action;
use crate::ui::component::file_explorer::FileExplorer;
use crate::ui::component::vertical_scrollbar::VerticalScrollbar;
#[cfg(test)]
use crate::ui::component::vertical_scrollbar::{SCROLLBAR_THUMB_SYMBOL, SCROLLBAR_TRACK_SYMBOL};
use crate::ui::diff_util::{
    DiffLine, DiffLineKind, FileTreeItem, diff_header_new_path, parse_diff_lines,
};
use crate::ui::{Component, Page, diff_util, style};

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
    all_files_summary: DiffSelectionChangeSummary,
    file_list_lines: Arc<[Line<'static>]>,
    key: DiffContentCacheKey,
    parsed_lines: Arc<[OwnedDiffLine]>,
    selection_summaries: Arc<[DiffSelectionChangeSummary]>,
    tree_items: Arc<[FileTreeItem]>,
}

/// Change totals for the currently selected diff tree item.
#[derive(Clone)]
struct DiffSelectionChangeSummary {
    added_lines: usize,
    label: String,
    removed_lines: usize,
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

    /// Returns the label and added/removed line totals for the active
    /// file-tree selection, or the whole diff when the selection is stale.
    fn selected_change_summary(&self, selected_index: usize) -> &DiffSelectionChangeSummary {
        if let Some(summary) = self.selection_summaries.get(selected_index) {
            return summary;
        }

        &self.all_files_summary
    }

    /// Returns parsed lines for the active file-tree selection.
    fn selected_lines(&self, selected_index: usize) -> Vec<DiffLine<'_>> {
        let parsed_lines = self.borrowed_lines();
        let Some(selected_item) = self.tree_items.get(selected_index) else {
            return parsed_lines;
        };

        diff_util::filter_diff_lines(&parsed_lines, selected_item)
    }

    /// Returns the complete parsed diff as borrowed lines.
    fn borrowed_lines(&self) -> Vec<DiffLine<'_>> {
        self.parsed_lines
            .iter()
            .map(OwnedDiffLine::borrowed)
            .collect()
    }

    /// Builds cached added/removed summaries for the full diff and each
    /// selectable tree item in one pass over parsed lines.
    fn change_summaries(
        parsed_lines: &[DiffLine<'_>],
        tree_items: &[FileTreeItem],
    ) -> (DiffSelectionChangeSummary, Vec<DiffSelectionChangeSummary>) {
        let mut all_files_totals = DiffChangeTotals::default();
        let mut current_path = None;
        let mut file_totals: HashMap<&str, DiffChangeTotals> = HashMap::new();

        for diff_line in parsed_lines {
            if diff_line.kind == DiffLineKind::FileHeader
                && diff_line.content.starts_with("diff --git")
            {
                current_path = diff_header_new_path(diff_line.content);

                continue;
            }

            let Some(line_totals) = DiffChangeTotals::from_line_kind(diff_line.kind) else {
                continue;
            };
            all_files_totals.add(line_totals);

            if let Some(path) = current_path {
                file_totals.entry(path).or_default().add(line_totals);
            }
        }

        let folder_totals = Self::folder_totals(&file_totals);
        let all_files_summary = DiffSelectionChangeSummary {
            added_lines: all_files_totals.added_lines,
            label: "all files".to_string(),
            removed_lines: all_files_totals.removed_lines,
        };
        let selection_summaries = tree_items
            .iter()
            .map(|item| {
                let totals = Self::tree_item_change_totals(item, &file_totals, &folder_totals);

                DiffSelectionChangeSummary {
                    added_lines: totals.added_lines,
                    label: tree_item_label(item),
                    removed_lines: totals.removed_lines,
                }
            })
            .collect();

        (all_files_summary, selection_summaries)
    }

    /// Aggregates file-level change totals into folder-prefix totals.
    fn folder_totals(
        file_totals: &HashMap<&str, DiffChangeTotals>,
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
        file_totals: &HashMap<&str, DiffChangeTotals>,
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
/// styled diff assembly so scroll metrics and frame painting reuse the same
/// rows for unchanged diff content, selection, panel width/height, scrollbar
/// gutter state, and active style version.
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
        let (all_files_summary, selection_summaries) =
            DiffContentSnapshot::change_summaries(&parsed_lines, &tree_items);
        let snapshot = DiffContentSnapshot {
            all_files_summary,
            file_list_lines: Arc::from(file_list_lines),
            key,
            parsed_lines: Arc::from(
                parsed_lines
                    .into_iter()
                    .map(OwnedDiffLine::from_diff_line)
                    .collect::<Vec<_>>(),
            ),
            selection_summaries: Arc::from(selection_summaries),
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
        let lines = DiffPage::build_diff_lines(&selected_lines, render_layout);
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
}

/// Renders the current session's git diff in a scrollable page.
pub struct DiffPage<'a> {
    /// Raw unified diff currently shown by the page.
    pub diff: &'a str,
    /// Shared cache for parsed diff content and rendered layouts.
    pub diff_layout_cache: &'a DiffLayoutCache,
    /// Selected file-tree row in the left panel.
    pub file_explorer_selected_index: usize,
    /// Vertical scroll offset inside the diff panel.
    pub scroll_offset: u16,
    /// Session whose diff is being rendered.
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
    /// Vertical scroll offset inside the diff panel.
    pub scroll_offset: u16,
    /// Session whose diff is being rendered.
    pub session: &'a Session,
}

impl<'a> DiffPage<'a> {
    /// Creates a diff page for the given session and scroll position.
    pub fn new(input: DiffPageInput<'a>) -> Self {
        let DiffPageInput {
            diff,
            diff_layout_cache,
            file_explorer_selected_index,
            scroll_offset,
            session,
        } = input;

        Self {
            diff,
            diff_layout_cache,
            file_explorer_selected_index,
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
        let selection_summary = content.selected_change_summary(self.file_explorer_selected_index);
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
            Span::styled("· ", Style::default().fg(style::palette::warning())),
            Span::styled(
                format!("{} ", inline_text(&selection_summary.label)),
                Style::default().fg(style::palette::text_muted()),
            ),
            Span::styled(
                format!("+{}", selection_summary.added_lines),
                Style::default().fg(style::palette::success()),
            ),
            Span::styled(" ", Style::default().fg(style::palette::warning())),
            Span::styled(
                format!("-{}", selection_summary.removed_lines),
                Style::default().fg(style::palette::danger()),
            ),
            Span::styled(" ", Style::default().fg(style::palette::warning())),
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
            let scrollbar_area =
                diff_util::diff_scrollbar_area(area, layout.render_layout.viewport_height);

            VerticalScrollbar::new(scroll_offset, layout.line_count).render(f, scrollbar_area);
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
            .map(text_util::borrowed_paint_line)
            .collect()
    }

    /// Builds wrapped diff lines for the diff panel, optionally reserving one
    /// column for the scrollbar thumb.
    fn build_diff_lines(
        parsed: &[DiffLine<'_>],
        layout: diff_util::DiffRenderLayout,
    ) -> Vec<Line<'static>> {
        let gutter_style = diff_util::body_diff_line_gutter_style();
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(parsed.len());

        for diff_line in parsed {
            if Self::append_special_diff_line(&mut lines, diff_line) {
                continue;
            }

            Self::append_body_diff_line(&mut lines, diff_line, layout, gutter_style);
        }

        if lines.is_empty() {
            lines.push(Line::from(" No changes found. "));
        }

        lines
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
) -> u16 {
    let diff_area = diff_util::diff_page_areas(terminal_area).diff_area;
    let content = diff_layout_cache.content(diff);
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

impl Page for DiffPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let areas = diff_util::diff_page_areas(area);
        let content = self.diff_layout_cache.content(self.diff);

        FileExplorer::from_cached_lines(content.file_list_lines())
            .selected_index(self.file_explorer_selected_index)
            .render(f, areas.file_list_area);

        self.render_diff_content(
            f,
            areas.diff_area,
            &content,
            self.session.stats.added_lines,
            self.session.stats.deleted_lines,
        );

        let help_message = Paragraph::new(crate::ui::help_format::footer_line(
            &help_action::diff_footer_actions(),
        ));
        f.render_widget(help_message, areas.footer_area);
    }
}

/// Returns a compact display label for one file-tree selection.
fn tree_item_label(item: &FileTreeItem) -> String {
    match item {
        FileTreeItem::Folder(path) | FileTreeItem::File(path) => path.clone(),
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
            scroll_offset,
            session,
        })
    }

    fn test_diff_layout_cache() -> &'static DiffLayoutCache {
        Box::leak(Box::new(DiffLayoutCache::default()))
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
        assert!(Arc::ptr_eq(
            &first_content.selection_summaries,
            &second_content.selection_summaries
        ));
    }

    #[test]
    fn test_diff_content_snapshot_caches_selection_change_summaries() {
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
        let folder_summary = content.selected_change_summary(0);
        let nested_folder_summary = content.selected_change_summary(1);
        let file_summary = content.selected_change_summary(3);
        let stale_summary = content.selected_change_summary(usize::MAX);

        // Assert
        assert_eq!(folder_summary.label, "src/");
        assert_eq!(folder_summary.added_lines, 2);
        assert_eq!(folder_summary.removed_lines, 1);
        assert_eq!(nested_folder_summary.label, "src/ui/");
        assert_eq!(nested_folder_summary.added_lines, 1);
        assert_eq!(nested_folder_summary.removed_lines, 0);
        assert_eq!(file_summary.label, "src/main.rs");
        assert_eq!(file_summary.added_lines, 1);
        assert_eq!(file_summary.removed_lines, 1);
        assert_eq!(stale_summary.label, "all files");
        assert_eq!(stale_summary.added_lines, 3);
        assert_eq!(stale_summary.removed_lines, 1);
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
        assert!(text.contains("src/ +1 -0"));
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
        assert!(text.contains("src/ +1 -0"));
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
