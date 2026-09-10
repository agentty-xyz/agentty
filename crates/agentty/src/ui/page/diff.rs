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
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::domain::session::Session;
use crate::presentation::app_mode::{
    DiffCommentTarget, DiffFocus, DiffLineComment, DiffLineCommentAnchor, DiffLineComments,
    DiffLineSide, DiffPreview, DiffPreviewUnavailableReason, DiffReviewComments, DiffSidebarFocus,
};
use crate::presentation::{help_action, review_comment as review_comment_selection};
use crate::ui::component::chat_input::ChatInput;
use crate::ui::component::file_explorer::FileExplorer;
use crate::ui::component::vertical_scrollbar::VerticalScrollbar;
use crate::ui::diff_util::{
    DiffLine, DiffLineKind, FileTreeItem, diff_header_new_path, diff_header_paths, parse_diff_lines,
};
use crate::ui::page::review_comment;
use crate::ui::{Component, Page, diff_util, input_layout, markdown, prompt_format, style};

const WRAPPED_CHUNK_START_INDEX: usize = 0;

const DIFF_COMMENT_CACHE_ENTRY_LIMIT: usize = 64;

const DIFF_CONTENT_CACHE_ENTRY_LIMIT: usize = 8;

const DIFF_LAYOUT_CACHE_ENTRY_LIMIT: usize = 16;

const FILE_LIST_CHANGE_TOTAL_SPAN_COUNT: usize = 4;

const COMMENT_INPUT_HORIZONTAL_MARGIN: usize = 1;

const COMMENT_INPUT_MAX_VISIBLE_LINES: usize = 5;

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

/// Cache key for one fully rendered file or inline comment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiffCommentCacheKey {
    comment_index: usize,
    content_width: usize,
    input_cursor: usize,
    input_revision: u64,
    is_editing: bool,
    is_selected: bool,
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

/// Parsed diff data reused by file-tree rendering and diff layout assembly.
#[derive(Clone)]
pub(crate) struct DiffContentSnapshot {
    file_line_ranges: Arc<HashMap<String, Vec<Range<usize>>>>,
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
        let path = self.selected_file_path(selected_index)?;
        let extension = Path::new(path).extension()?.to_str()?;
        if !extension.eq_ignore_ascii_case("md") {
            return None;
        }

        Some(path)
    }

    /// Returns the repository-relative path for the selected file row.
    pub(crate) fn selected_file_path(&self, selected_index: usize) -> Option<&str> {
        let FileTreeItem::File(path) = self.tree_items.get(selected_index)? else {
            return None;
        };

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

    /// Returns the selected changed line with its old- or new-side file path.
    pub(crate) fn selected_changed_line(
        &self,
        selected_index: usize,
        changed_line_index: usize,
    ) -> Option<DiffLineCommentAnchor> {
        self.find_changed_line(
            selected_index,
            |current_changed_line_index, diff_line, side, line_number, path| {
                (current_changed_line_index == changed_line_index).then(|| DiffLineCommentAnchor {
                    content: diff_line.content.to_string(),
                    line: line_number,
                    path: path.to_string(),
                    side,
                })
            },
        )
    }

    /// Returns changed-line anchors within one inclusive visual selection.
    pub(crate) fn selected_changed_lines(
        &self,
        selected_index: usize,
        start_changed_line_index: usize,
        end_changed_line_index: usize,
    ) -> Vec<DiffLineCommentAnchor> {
        if start_changed_line_index > end_changed_line_index {
            return Vec::new();
        }
        let Some(FileTreeItem::File(fallback_path)) = self.tree_items.get(selected_index) else {
            return Vec::new();
        };
        let mut anchors = Vec::new();
        let mut current_changed_line_index = 0;
        let mut current_paths: Option<(String, String)> = None;

        for diff_line in self.selected_lines(selected_index) {
            if diff_line.kind == DiffLineKind::FileHeader {
                if let Some(paths) = diff_header_paths(diff_line.content) {
                    current_paths = Some(paths);
                }

                continue;
            }
            let Some((side, line, path)) =
                Self::line_comment_location(&diff_line, current_paths.as_ref(), fallback_path)
            else {
                continue;
            };
            if current_changed_line_index > end_changed_line_index {
                break;
            }
            if current_changed_line_index >= start_changed_line_index {
                anchors.push(DiffLineCommentAnchor {
                    content: diff_line.content.to_string(),
                    line,
                    path: path.to_string(),
                    side,
                });
            }
            current_changed_line_index = current_changed_line_index.saturating_add(1);
        }

        anchors
    }

    /// Returns the changed-line cursor index that owns `anchor`.
    pub(crate) fn changed_line_index_for_anchor(
        &self,
        selected_index: usize,
        anchor: &DiffLineCommentAnchor,
    ) -> Option<usize> {
        self.find_changed_line(
            selected_index,
            |current_changed_line_index, diff_line, side, line_number, path| {
                (side == anchor.side
                    && line_number == anchor.line
                    && path == anchor.path.as_str()
                    && diff_line.content == anchor.content)
                    .then_some(current_changed_line_index)
            },
        )
    }

    /// Finds one changed line while tracking its hunk counter and side path.
    fn find_changed_line<T>(
        &self,
        selected_index: usize,
        mut find: impl FnMut(usize, &DiffLine<'_>, DiffLineSide, u32, &str) -> Option<T>,
    ) -> Option<T> {
        let FileTreeItem::File(fallback_path) = self.tree_items.get(selected_index)? else {
            return None;
        };
        let mut current_paths = None;
        let mut current_changed_line_index = 0;

        for diff_line in self.selected_lines(selected_index) {
            if diff_line.kind == DiffLineKind::FileHeader
                && diff_line.content.starts_with("diff --git")
            {
                current_paths = diff_header_paths(diff_line.content);

                continue;
            }
            let Some((side, line_number, path)) =
                Self::line_comment_location(&diff_line, current_paths.as_ref(), fallback_path)
            else {
                continue;
            };
            if let Some(found) = find(
                current_changed_line_index,
                &diff_line,
                side,
                line_number,
                path,
            ) {
                return Some(found);
            }
            current_changed_line_index = current_changed_line_index.saturating_add(1);
        }

        None
    }

    /// Resolves one changed line's side-specific path and line number.
    fn line_comment_location<'path>(
        diff_line: &DiffLine<'_>,
        current_paths: Option<&'path (String, String)>,
        fallback_path: &'path str,
    ) -> Option<(DiffLineSide, u32, &'path str)> {
        match diff_line.kind {
            DiffLineKind::Addition => Some((
                DiffLineSide::New,
                diff_line.new_line?,
                current_paths.map_or(fallback_path, |(_, new_path)| new_path),
            )),
            DiffLineKind::Deletion => Some((
                DiffLineSide::Old,
                diff_line.old_line?,
                current_paths.map_or(fallback_path, |(old_path, _)| old_path),
            )),
            DiffLineKind::Context | DiffLineKind::FileHeader | DiffLineKind::HunkHeader => None,
        }
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

/// One diff comment row inserted adjacent to its file or changed rows.
#[derive(Clone)]
struct DiffLineCommentInsertion {
    comment: usize,
    display_row: usize,
    end_changed_line_index: usize,
    height: usize,
    highlight_changed_line_bounds: Option<(usize, usize)>,
    lines: Arc<[Line<'static>]>,
    source_end: usize,
}

/// Short-lived inputs used to paint one commented diff viewport.
struct DiffVisibleLineRequest<'a> {
    comment_highlight_ranges: &'a [Range<usize>],
    comment_insertions: &'a [DiffLineCommentInsertion],
    scroll_offset: u16,
    selected_range: Option<&'a Range<usize>>,
    viewport_height: u16,
}

/// Final diff layout selected for the current panel and scrollbar state.
#[derive(Clone)]
pub(crate) struct DiffResolvedLayout {
    pub(crate) changed_line_ranges: Arc<[Range<usize>]>,
    pub(crate) line_count: usize,
    pub(crate) lines: Arc<[Line<'static>]>,
    pub(crate) render_layout: diff_util::DiffRenderLayout,
    pub(crate) show_scrollbar: bool,
    comment_insertions: Vec<DiffLineCommentInsertion>,
}

impl DiffResolvedLayout {
    /// Adds file and inline comment rows to one cached source layout.
    fn with_line_comments(
        cached_layout: DiffCachedLayout,
        content: &DiffContentSnapshot,
        selected_file_index: usize,
        line_comments: &DiffLineComments,
        diff_layout_cache: &DiffLayoutCache,
    ) -> Self {
        let comment_insertions = diff_line_comment_insertions(
            content,
            selected_file_index,
            &cached_layout.changed_line_ranges,
            line_comments,
            cached_layout.render_layout.content_width,
            diff_layout_cache,
        );
        let changed_line_ranges = changed_line_ranges_with_comments(
            &cached_layout.changed_line_ranges,
            &comment_insertions,
        );
        let line_count = cached_layout.line_count.saturating_add(
            comment_insertions
                .iter()
                .map(|insertion| insertion.height)
                .sum(),
        );
        let show_scrollbar = diff_util::diff_has_scrollable_overflow(
            line_count,
            cached_layout.render_layout.viewport_height,
        );

        Self {
            changed_line_ranges,
            comment_insertions,
            line_count,
            lines: cached_layout.lines,
            render_layout: cached_layout.render_layout,
            show_scrollbar,
        }
    }

    /// Returns the number of rendered addition and deletion source lines.
    pub(crate) fn changed_line_count(&self) -> usize {
        self.changed_line_ranges.len()
    }

    /// Returns the next selectable source or diff-comment row.
    pub(crate) fn next_content_selection(
        &self,
        selected_changed_line_index: usize,
        selected_comment_index: Option<usize>,
    ) -> (usize, Option<usize>) {
        self.adjacent_content_selection(selected_changed_line_index, selected_comment_index, true)
    }

    /// Returns the previous selectable source or diff-comment row.
    pub(crate) fn previous_content_selection(
        &self,
        selected_changed_line_index: usize,
        selected_comment_index: Option<usize>,
    ) -> (usize, Option<usize>) {
        self.adjacent_content_selection(selected_changed_line_index, selected_comment_index, false)
    }

    /// Returns the first changed source line intersecting or below one visual
    /// row in the current viewport.
    pub(crate) fn changed_line_index_at_visual_row(
        &self,
        scroll_offset: u16,
        visual_row: u16,
    ) -> Option<usize> {
        let target_row = usize::from(scroll_offset).saturating_add(usize::from(visual_row));

        self.changed_line_ranges
            .iter()
            .position(|range| range.end > target_row)
            .or_else(|| self.changed_line_ranges.len().checked_sub(1))
    }

    /// Returns the scroll offset that keeps one changed source line visible.
    pub(crate) fn changed_line_scroll_offset(
        &self,
        selected_diff_line_index: usize,
        current_scroll_offset: u16,
    ) -> Option<u16> {
        let selected_range = self.changed_line_ranges.get(selected_diff_line_index)?;

        Some(self.range_scroll_offset(selected_range, current_scroll_offset))
    }

    /// Returns the scroll offset that keeps the selected source or comment row
    /// visible.
    pub(crate) fn content_selection_scroll_offset(
        &self,
        selected_changed_line_index: usize,
        selected_comment_index: Option<usize>,
        current_scroll_offset: u16,
    ) -> Option<u16> {
        let selected_range =
            self.content_selection_range(selected_changed_line_index, selected_comment_index)?;

        Some(self.range_scroll_offset(&selected_range, current_scroll_offset))
    }

    /// Returns the rendered rows covered by inclusive changed-line bounds.
    fn changed_line_selection_range(
        &self,
        start_changed_line_index: usize,
        end_changed_line_index: usize,
    ) -> Option<Range<usize>> {
        let start_range = self.changed_line_ranges.get(start_changed_line_index)?;
        let end_range = self.changed_line_ranges.get(end_changed_line_index)?;

        Some(start_range.start..end_range.end)
    }

    /// Returns the rendered range for the selected source or diff-comment
    /// row.
    fn content_selection_range(
        &self,
        selected_changed_line_index: usize,
        selected_comment_index: Option<usize>,
    ) -> Option<Range<usize>> {
        if let Some(comment_index) = selected_comment_index
            && let Some(insertion) = self
                .comment_insertions
                .iter()
                .find(|insertion| insertion.comment == comment_index)
        {
            return Some(
                insertion.display_row..insertion.display_row.saturating_add(insertion.height),
            );
        }

        self.changed_line_ranges
            .get(selected_changed_line_index)
            .cloned()
    }

    /// Moves one step through source rows and inserted comment rows.
    fn adjacent_content_selection(
        &self,
        selected_changed_line_index: usize,
        selected_comment_index: Option<usize>,
        move_next: bool,
    ) -> (usize, Option<usize>) {
        let selections = self.content_selections();
        let current_index = selections
            .iter()
            .position(|selection| {
                selected_comment_index.map_or_else(
                    || {
                        selection.changed_line_index == selected_changed_line_index
                            && selection.comment_index.is_none()
                    },
                    |comment_index| selection.comment_index == Some(comment_index),
                )
            })
            .or_else(|| {
                selections.iter().position(|selection| {
                    selection.changed_line_index == selected_changed_line_index
                        && selection.comment_index.is_none()
                })
            })
            .unwrap_or_default();
        let next_index = if move_next {
            current_index
                .saturating_add(1)
                .min(selections.len().saturating_sub(1))
        } else {
            current_index.saturating_sub(1)
        };

        selections
            .get(next_index)
            .map_or((selected_changed_line_index, None), |selection| {
                (selection.changed_line_index, selection.comment_index)
            })
    }

    /// Builds selectable content rows in their rendered order.
    fn content_selections(&self) -> Vec<DiffContentSelection> {
        let mut selections = self
            .changed_line_ranges
            .iter()
            .enumerate()
            .map(|(changed_line_index, range)| DiffContentSelection {
                changed_line_index,
                comment_index: None,
                display_row: range.start,
            })
            .chain(
                self.comment_insertions
                    .iter()
                    .map(|insertion| DiffContentSelection {
                        changed_line_index: insertion.end_changed_line_index,
                        comment_index: Some(insertion.comment),
                        display_row: insertion.display_row,
                    }),
            )
            .collect::<Vec<_>>();
        selections.sort_unstable_by_key(|selection| selection.display_row);

        selections
    }

    /// Keeps one rendered range inside the current viewport.
    fn range_scroll_offset(
        &self,
        selected_range: &Range<usize>,
        current_scroll_offset: u16,
    ) -> u16 {
        let viewport_height = usize::from(self.render_layout.viewport_height);
        if viewport_height == 0 {
            return 0;
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

        diff_util::clamp_diff_scroll_offset(
            next_scroll_offset,
            self.line_count,
            self.render_layout.viewport_height,
        )
    }

    /// Returns rendered source ranges owned by visible inline comments.
    fn line_comment_highlight_ranges(&self) -> Vec<Range<usize>> {
        self.comment_insertions
            .iter()
            .filter_map(|insertion| {
                let (start_changed_line_index, end_changed_line_index) =
                    insertion.highlight_changed_line_bounds?;

                self.changed_line_selection_range(start_changed_line_index, end_changed_line_index)
            })
            .collect()
    }
}

/// One selectable source or diff-comment row in rendered order.
struct DiffContentSelection {
    changed_line_index: usize,
    comment_index: Option<usize>,
    display_row: usize,
}

/// Resolves diff comment rows and their display positions for one file.
fn diff_line_comment_insertions(
    content: &DiffContentSnapshot,
    selected_file_index: usize,
    changed_line_ranges: &[Range<usize>],
    line_comments: &DiffLineComments,
    content_width: usize,
    diff_layout_cache: &DiffLayoutCache,
) -> Vec<DiffLineCommentInsertion> {
    let mut insertions = line_comments
        .comments
        .iter()
        .enumerate()
        .filter_map(|(comment_index, line_comment)| {
            let (insertion_index, end_changed_line_index, highlight_changed_line_bounds) =
                match &line_comment.target {
                    DiffCommentTarget::File { path }
                        if content.selected_file_path(selected_file_index)
                            == Some(path.as_str()) =>
                    {
                        (0, 0, None)
                    }
                    DiffCommentTarget::File { .. } => return None,
                    DiffCommentTarget::Lines(target) => {
                        let start_changed_line_index = content.changed_line_index_for_anchor(
                            selected_file_index,
                            target.first_anchor(),
                        )?;
                        let end_changed_line_index = content.changed_line_index_for_anchor(
                            selected_file_index,
                            target.last_anchor(),
                        )?;
                        let insertion_index = changed_line_ranges.get(end_changed_line_index)?.end;

                        (
                            insertion_index,
                            end_changed_line_index,
                            Some((start_changed_line_index, end_changed_line_index)),
                        )
                    }
                };

            let lines = diff_layout_cache.comment_lines(
                comment_index,
                line_comment,
                line_comments.editing_index == Some(comment_index),
                line_comments.selected_comment_index() == Some(comment_index),
                content_width,
            );
            let height = lines.len();

            Some((
                insertion_index,
                comment_index,
                end_changed_line_index,
                height,
                highlight_changed_line_bounds,
                lines,
            ))
        })
        .collect::<Vec<_>>();
    insertions.sort_unstable_by_key(|(insertion_index, ..)| *insertion_index);

    insertions
        .into_iter()
        .scan(0usize, |preceding_comment_height, insertion| {
            let (
                insertion_index,
                comment,
                end_changed_line_index,
                height,
                highlight_changed_line_bounds,
                lines,
            ) = insertion;
            let display_row = insertion_index.saturating_add(*preceding_comment_height);
            *preceding_comment_height = preceding_comment_height.saturating_add(height);

            Some(DiffLineCommentInsertion {
                comment,
                display_row,
                end_changed_line_index,
                height,
                highlight_changed_line_bounds,
                lines,
                source_end: insertion_index,
            })
        })
        .collect()
}

/// Adjusts cached changed-line ranges for comment rows inserted before them.
fn changed_line_ranges_with_comments(
    changed_line_ranges: &[Range<usize>],
    insertions: &[DiffLineCommentInsertion],
) -> Arc<[Range<usize>]> {
    changed_line_ranges
        .iter()
        .map(|range| {
            let preceding_comment_height = insertions
                .iter()
                .filter(|insertion| insertion.source_end <= range.start)
                .map(|insertion| insertion.height)
                .sum::<usize>();

            range.start.saturating_add(preceding_comment_height)
                ..range.end.saturating_add(preceding_comment_height)
        })
        .collect()
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

/// Cached rows for one complete diff comment render snapshot.
struct DiffCommentCacheEntry {
    input_text: Arc<str>,
    key: DiffCommentCacheKey,
    lines: Arc<[Line<'static>]>,
    target: DiffCommentTarget,
}

impl Default for DiffLayoutCache {
    fn default() -> Self {
        Self {
            comment_rows: RefCell::new(VecDeque::with_capacity(DIFF_COMMENT_CACHE_ENTRY_LIMIT)),
            content_entries: RefCell::new(VecDeque::with_capacity(DIFF_CONTENT_CACHE_ENTRY_LIMIT)),
            layout_entries: RefCell::new(VecDeque::with_capacity(DIFF_LAYOUT_CACHE_ENTRY_LIMIT)),
        }
    }
}

/// Bounded cache for parsed diff content and fully assembled diff layouts.
///
/// The parsed-content layer avoids re-parsing the same raw diff and rebuilding
/// file-tree metadata or per-path line ranges on every frame. Its key includes
/// the raw diff's hash and byte length plus the active style version, so
/// replacing the diff or theme invalidates the styled snapshot. The
/// rendered-layout layer sits above styled diff assembly so scroll metrics
/// and frame painting reuse the same rows until diff content, selection,
/// panel width/height, scrollbar gutter state, or the active style version
/// changes. The comment layer retains rows by input snapshot, width, target,
/// interaction state, and style version so height measurement and painting
/// share one render. Every LRU layer evicts its oldest entries at its fixed
/// limit.
pub struct DiffLayoutCache {
    comment_rows: RefCell<VecDeque<DiffCommentCacheEntry>>,
    content_entries: RefCell<VecDeque<DiffContentCacheEntry>>,
    layout_entries: RefCell<VecDeque<DiffLayoutCacheEntry>>,
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
        line_comments: &DiffLineComments,
        selected_index: usize,
        diff_area: Rect,
    ) -> DiffResolvedLayout {
        let layout_without_scrollbar = self.layout(DiffLayoutRequest {
            content,
            diff_area,
            reserve_scrollbar_width: false,
            selected_index,
        });
        let resolved_without_scrollbar = DiffResolvedLayout::with_line_comments(
            layout_without_scrollbar,
            content,
            selected_index,
            line_comments,
            self,
        );
        if !resolved_without_scrollbar.show_scrollbar {
            return resolved_without_scrollbar;
        }

        let layout_with_scrollbar = self.layout(DiffLayoutRequest {
            content,
            diff_area,
            reserve_scrollbar_width: true,
            selected_index,
        });
        DiffResolvedLayout::with_line_comments(
            layout_with_scrollbar,
            content,
            selected_index,
            line_comments,
            self,
        )
    }

    /// Returns cached comment rows or renders and stores one input snapshot.
    fn comment_lines(
        &self,
        comment_index: usize,
        comment: &DiffLineComment,
        is_editing: bool,
        is_selected: bool,
        content_width: usize,
    ) -> Arc<[Line<'static>]> {
        let key = DiffCommentCacheKey {
            comment_index,
            content_width,
            input_cursor: comment.input.cursor,
            input_revision: comment.input.revision(),
            is_editing,
            is_selected,
            style_version: style::active_theme_cache_version(),
        };
        if let Some(lines) = self.cached_comment_lines(&key, comment) {
            return lines;
        }

        let lines = Arc::from(DiffPage::inline_comment_lines(
            comment,
            is_editing,
            is_selected,
            content_width,
        ));
        self.store_comment_lines(DiffCommentCacheEntry {
            input_text: Arc::from(comment.input.text()),
            key,
            lines: Arc::clone(&lines),
            target: comment.target.clone(),
        });

        lines
    }

    /// Returns cached comment rows and promotes the entry in the LRU queue.
    fn cached_comment_lines(
        &self,
        key: &DiffCommentCacheKey,
        comment: &DiffLineComment,
    ) -> Option<Arc<[Line<'static>]>> {
        let mut entries = self.comment_rows.borrow_mut();
        let entry_index = entries.iter().position(|entry| {
            &entry.key == key
                && entry.input_text.as_ref() == comment.input.text()
                && entry.target == comment.target
        })?;
        let entry = entries.remove(entry_index)?;
        let lines = Arc::clone(&entry.lines);
        entries.push_front(entry);

        Some(lines)
    }

    /// Stores rendered comment rows and evicts entries over the fixed limit.
    fn store_comment_lines(&self, entry: DiffCommentCacheEntry) {
        let mut entries = self.comment_rows.borrow_mut();
        entries.push_front(entry);

        while entries.len() > DIFF_COMMENT_CACHE_ENTRY_LIMIT {
            entries.pop_back();
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

/// Borrowed inputs required to construct a [`DiffPage`] for one frame.
#[derive(Clone, Copy)]
pub struct DiffPageInput<'a> {
    /// Whether this session may collect diff comments for a reply.
    pub can_comment: bool,
    /// Raw unified diff currently shown by the page.
    pub diff: &'a str,
    /// Shared cache for parsed diff content and rendered diff layouts.
    pub diff_layout_cache: &'a DiffLayoutCache,
    /// Selected file-tree row in the left panel.
    pub file_explorer_selected_index: usize,
    /// Panel currently receiving changed-file navigation input.
    pub focus: DiffFocus,
    /// File and inline comments accumulated for the next turn.
    pub line_comments: &'a DiffLineComments,
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

/// Renders the current session's git diff in a scrollable page.
pub struct DiffPage<'a> {
    /// Whether this session may collect diff comments for a reply.
    pub can_comment: bool,
    /// Raw unified diff currently shown by the page.
    pub diff: &'a str,
    /// Shared cache for parsed diff content and rendered layouts.
    pub diff_layout_cache: &'a DiffLayoutCache,
    /// Selected file-tree row in the left panel.
    pub file_explorer_selected_index: usize,
    /// Panel currently receiving changed-file navigation input.
    pub focus: DiffFocus,
    /// File and inline comments accumulated for the next turn.
    pub line_comments: &'a DiffLineComments,
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
    is_loading: bool,
}

impl<'a> DiffPage<'a> {
    /// Creates a diff page for the given session and scroll position.
    pub fn new(input: DiffPageInput<'a>) -> Self {
        let DiffPageInput {
            can_comment,
            diff,
            diff_layout_cache,
            file_explorer_selected_index,
            focus,
            line_comments,
            markdown_render_cache,
            preview,
            review_comments,
            scroll_offset,
            selected_diff_line_index,
            session,
            sidebar_focus,
        } = input;

        Self {
            can_comment,
            diff,
            diff_layout_cache,
            file_explorer_selected_index,
            focus,
            line_comments,
            markdown_render_cache,
            preview,
            review_comments,
            scroll_offset,
            selected_diff_line_index,
            session,
            sidebar_focus,
            is_loading: false,
        }
    }

    /// Marks this page as a pending diff load with an explicit sidebar
    /// placeholder.
    pub(crate) fn loading(mut self) -> Self {
        self.is_loading = true;

        self
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
            self.line_comments,
            self.file_explorer_selected_index,
            area,
        );

        let scroll_offset = diff_util::clamp_diff_scroll_offset(
            self.scroll_offset,
            layout.line_count,
            layout.render_layout.viewport_height,
        );
        let comment_highlight_ranges = layout.line_comment_highlight_ranges();
        let selected_range = (self.focus == DiffFocus::Content)
            .then(|| {
                if self.line_comments.is_selecting() {
                    let (start_changed_line_index, end_changed_line_index) = self
                        .line_comments
                        .selected_row_bounds(self.selected_diff_line_index);

                    return layout.changed_line_selection_range(
                        start_changed_line_index,
                        end_changed_line_index,
                    );
                }

                layout.content_selection_range(
                    self.selected_diff_line_index,
                    self.line_comments.selected_comment_index(),
                )
            })
            .flatten();
        let paint_lines = Self::borrowed_visible_lines_with_comments(
            &layout.lines,
            &DiffVisibleLineRequest {
                comment_insertions: &layout.comment_insertions,
                comment_highlight_ranges: &comment_highlight_ranges,
                scroll_offset,
                selected_range: selected_range.as_ref(),
                viewport_height: layout.render_layout.viewport_height,
            },
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

    /// Builds visible rows from cached diff lines plus short-lived comments.
    fn borrowed_visible_lines_with_comments<'line>(
        lines: &'line [Line<'static>],
        request: &DiffVisibleLineRequest<'line>,
    ) -> Vec<Line<'line>> {
        let comment_height = request
            .comment_insertions
            .iter()
            .map(|insertion| insertion.height)
            .sum::<usize>();
        let line_count = lines.len().saturating_add(comment_height);
        let start_index = usize::from(request.scroll_offset).min(line_count);
        let end_index = start_index
            .saturating_add(usize::from(request.viewport_height))
            .min(line_count);

        (start_index..end_index)
            .filter_map(|display_row| {
                if let Some(insertion) = request.comment_insertions.iter().find(|insertion| {
                    (insertion.display_row..insertion.display_row.saturating_add(insertion.height))
                        .contains(&display_row)
                }) {
                    let comment_line = insertion
                        .lines
                        .get(display_row.saturating_sub(insertion.display_row))?;

                    return Some(text_util::borrowed_paint_line(comment_line));
                }

                let preceding_comment_height = request
                    .comment_insertions
                    .iter()
                    .filter(|insertion| insertion.display_row < display_row)
                    .map(|insertion| insertion.height)
                    .sum::<usize>();
                let original_index = display_row.saturating_sub(preceding_comment_height);
                let mut paint_line = text_util::borrowed_paint_line(lines.get(original_index)?);
                if request
                    .comment_highlight_ranges
                    .iter()
                    .any(|range| range.contains(&display_row))
                {
                    paint_line.style = paint_line.style.bg(style::palette::surface_prompt());
                    for span in &mut paint_line.spans {
                        span.style = span.style.bg(style::palette::surface_prompt());
                    }
                }
                if request
                    .selected_range
                    .is_some_and(|range| range.contains(&display_row))
                {
                    paint_line.style = paint_line.style.add_modifier(Modifier::REVERSED);
                    for span in &mut paint_line.spans {
                        span.style = span.style.add_modifier(Modifier::REVERSED);
                    }
                }

                Some(paint_line)
            })
            .collect()
    }

    /// Builds one bordered multiline comment editor with a contextual title.
    fn inline_comment_lines(
        comment: &DiffLineComment,
        is_editing: bool,
        is_selected: bool,
        content_width: usize,
    ) -> Vec<Line<'static>> {
        let background = if is_editing {
            style::palette::surface_selection()
        } else {
            style::palette::surface_prompt()
        };
        let selected_modifier = if is_selected && !is_editing {
            Modifier::REVERSED
        } else {
            Modifier::empty()
        };
        let content_style = Style::default()
            .fg(style::palette::text())
            .bg(background)
            .add_modifier(selected_modifier);
        let chrome_style = Style::default()
            .fg(style::palette::accent())
            .bg(background)
            .add_modifier(Modifier::BOLD | selected_modifier);
        let comment_margin = COMMENT_INPUT_HORIZONTAL_MARGIN.min(content_width);
        let box_width = content_width.saturating_sub(comment_margin);
        if box_width < 5 {
            return vec![Self::padded_comment_line(
                vec![Span::styled(" ".repeat(box_width), content_style)],
                comment_margin,
                content_width,
                content_style,
            )];
        }

        let title = Self::inline_comment_title(&comment.target);
        let title = text_util::truncate_with_ellipsis(&title, box_width.saturating_sub(5));
        let title_width = UnicodeWidthStr::width(title.as_str());
        let top_fill_width = box_width.saturating_sub(title_width.saturating_add(5));
        let top_border = vec![
            Span::styled("╭─ ", chrome_style),
            Span::styled(title, chrome_style),
            Span::styled(format!(" {}╮", "─".repeat(top_fill_width)), chrome_style),
        ];
        let body_width = box_width.saturating_sub(4).max(1);
        let (body_lines, cursor_row) =
            wrapped_comment_input(&comment.input, is_editing, body_width);
        let visible_line_count = if is_editing {
            body_lines.len().min(COMMENT_INPUT_MAX_VISIBLE_LINES)
        } else {
            body_lines.len()
        };
        let body_start = if is_editing {
            cursor_row.saturating_sub(visible_line_count.saturating_sub(1))
        } else {
            0
        }
        .min(body_lines.len().saturating_sub(visible_line_count));
        let bottom_border = vec![Span::styled(
            format!("╰{}╯", "─".repeat(box_width.saturating_sub(2))),
            chrome_style,
        )];
        let mut lines = Vec::with_capacity(visible_line_count.saturating_add(2));
        lines.push(Self::padded_comment_line(
            top_border,
            comment_margin,
            content_width,
            content_style,
        ));
        lines.extend(
            body_lines
                .into_iter()
                .skip(body_start)
                .take(visible_line_count)
                .map(|body| {
                    let body_padding =
                        body_width.saturating_sub(UnicodeWidthStr::width(body.as_str()));

                    Self::padded_comment_line(
                        vec![
                            Span::styled("│ ", chrome_style),
                            Span::styled(body, content_style),
                            Span::styled(format!("{} │", " ".repeat(body_padding)), chrome_style),
                        ],
                        comment_margin,
                        content_width,
                        content_style,
                    )
                }),
        );
        lines.push(Self::padded_comment_line(
            bottom_border,
            comment_margin,
            content_width,
            content_style,
        ));

        lines
    }

    /// Returns the title shown in one file or inline comment editor.
    fn inline_comment_title(target: &DiffCommentTarget) -> String {
        let DiffCommentTarget::Lines(target) = target else {
            return "File comment".to_string();
        };

        [(DiffLineSide::Old, "Old"), (DiffLineSide::New, "New")]
            .into_iter()
            .filter_map(|(side, label)| {
                target.line_bounds(side).map(|(first_line, last_line)| {
                    if first_line == last_line {
                        format!("{label} line {first_line}")
                    } else {
                        format!("{label} lines {first_line}-{last_line}")
                    }
                })
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Adds the editor margin and full-width background to a comment row.
    fn padded_comment_line(
        spans: Vec<Span<'static>>,
        comment_margin: usize,
        content_width: usize,
        content_style: Style,
    ) -> Line<'static> {
        let mut spans = std::iter::once(Span::styled(" ".repeat(comment_margin), content_style))
            .chain(spans)
            .collect::<Vec<_>>();
        let line_width = spans.iter().map(Span::width).sum::<usize>();
        if line_width < content_width {
            spans.push(Span::styled(
                " ".repeat(content_width - line_width),
                content_style,
            ));
        }

        Line::from(spans)
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
    line_comments: &DiffLineComments,
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
    let layout =
        diff_layout_cache.resolved_layout(&content, line_comments, selected_index, diff_area);
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
    line_comments: &DiffLineComments,
    selected_file_index: usize,
    terminal_area: Rect,
    diff_layout_cache: &DiffLayoutCache,
) -> DiffResolvedLayout {
    let diff_area = diff_util::diff_page_areas(terminal_area).diff_area;
    let content = diff_layout_cache.content(diff);

    diff_layout_cache.resolved_layout(&content, line_comments, selected_file_index, diff_area)
}

impl DiffPage<'_> {
    /// Builds the Files sidebar for either a pending or completed diff load.
    fn file_explorer(&self, content: &DiffContentSnapshot) -> FileExplorer {
        if self.is_loading {
            return FileExplorer::loading();
        }

        FileExplorer::from_cached_lines(
            content.file_list_lines(),
            FILE_LIST_CHANGE_TOTAL_SPAN_COUNT,
        )
        .selected_index(self.file_explorer_selected_index)
        .focused(self.sidebar_focus == DiffSidebarFocus::Files && self.focus == DiffFocus::Files)
    }

    /// Paints repository suggestions adjacent to the active comment input.
    fn render_comment_lookup(&self, f: &mut Frame, diff_area: Rect, footer_area: Rect) {
        let Some(state) = &self.line_comments.at_mention_state else {
            return;
        };
        let Some(index) = self.line_comments.editing_index else {
            return;
        };
        let content = self.diff_layout_cache.content(self.diff);
        let layout = self.diff_layout_cache.resolved_layout(
            &content,
            self.line_comments,
            self.file_explorer_selected_index,
            diff_area,
        );
        let scroll_offset = diff_util::clamp_diff_scroll_offset(
            self.scroll_offset,
            layout.line_count,
            layout.render_layout.viewport_height,
        );
        let viewport = Rect::new(
            diff_area.x + 1,
            diff_area.y + 1,
            u16::try_from(layout.render_layout.content_width).unwrap_or(u16::MAX),
            layout.render_layout.viewport_height,
        );
        let lookup_area = |height| {
            layout
                .comment_insertions
                .iter()
                .find(|insertion| insertion.comment == index)
                .and_then(|insertion| {
                    comment_lookup_area(
                        viewport,
                        insertion.display_row..insertion.display_row + insertion.height,
                        scroll_offset,
                        height,
                    )
                })
        };
        let menu = lookup_area(12).and_then(|area| {
            let input = &self.line_comments.comments[index].input;
            prompt_format::file_lookup_suggestion_list(
                input.text(),
                input.cursor,
                state,
                usize::from(area.height.saturating_sub(2)),
            )
        });
        let help = if let Some(menu) = menu
            && let Some(area) =
                lookup_area(input_layout::suggestion_dropdown_height(menu.items.len()))
        {
            ChatInput::render_suggestion_dropdown(f, area, &menu);
            "Up/Down: navigate  Tab/Enter: select  Esc: dismiss lookup"
        } else {
            "Esc: dismiss lookup"
        };
        f.render_widget(Paragraph::new(help), footer_area);
    }
}

impl Page for DiffPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let areas = diff_util::diff_page_areas(area);
        let content = self.diff_layout_cache.content(self.diff);
        let sidebar_areas =
            diff_util::diff_sidebar_areas(areas.file_list_area, self.review_comments.is_some());

        self.file_explorer(&content)
            .render(f, sidebar_areas.file_list_area);

        let review_comment_page = self.review_comments.map(|review_comments| {
            let rows = review_comments
                .comment_snapshot
                .as_ref()
                .map(review_comment_selection::grouped_review_comment_rows)
                .unwrap_or_default();
            let page =
                review_comment::ReviewCommentPage::new(review_comment::ReviewCommentPageInput {
                    selected_comments: &review_comments.selected_comments,
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
                        can_reply && !review_comments.selected_comments.is_empty(),
                    )
                })
        } else {
            (false, false)
        };
        let help_message = Paragraph::new(crate::ui::help_format::footer_line(
            &help_action::diff_footer_actions(help_action::DiffFooterContext {
                can_mark_selected,
                can_submit,
                file_comment: help_action::DiffFileCommentAvailability::from_bool(
                    self.can_comment
                        && content.selected_item_is_file(self.file_explorer_selected_index),
                ),
                focus: self.focus,
                has_review_comments: self.review_comments.is_some(),
                line_comment_state: if !self.can_comment {
                    help_action::DiffLineCommentFooterState::ReadOnly
                } else if self.line_comments.is_editing() {
                    help_action::DiffLineCommentFooterState::Editing
                } else if self.line_comments.is_selecting() {
                    help_action::DiffLineCommentFooterState::Selecting
                } else {
                    help_action::DiffLineCommentFooterState::Ready {
                        comment_count: self.line_comments.comments.len(),
                    }
                },
                sidebar_focus: self.sidebar_focus,
            }),
        ));
        f.render_widget(help_message, areas.footer_area);
        self.render_comment_lookup(f, areas.diff_area, areas.footer_area);
    }
}

/// Anchors the lookup to the visible comment, preferring the space above it.
/// Top-edge comments use the space below rather than covering their own input.
fn comment_lookup_area(
    viewport: Rect,
    rows: Range<usize>,
    scroll_offset: u16,
    desired_height: u16,
) -> Option<Rect> {
    let scroll_offset = usize::from(scroll_offset);
    if rows.end <= scroll_offset || rows.start >= scroll_offset + usize::from(viewport.height) {
        return None;
    }
    let top = u16::try_from(rows.start.saturating_sub(scroll_offset)).unwrap_or(u16::MAX);
    let bottom = u16::try_from(rows.end.saturating_sub(scroll_offset))
        .unwrap_or(u16::MAX)
        .min(viewport.height);
    let margin = u16::try_from(COMMENT_INPUT_HORIZONTAL_MARGIN)
        .unwrap_or(u16::MAX)
        .min(viewport.width);
    let input_area = Rect::new(
        viewport.x + margin,
        viewport.y + top,
        viewport.width - margin,
        bottom.saturating_sub(top),
    );
    if top >= 3 {
        return input_layout::overlay_area_above(viewport, input_area, desired_height);
    }
    let height = viewport.height.saturating_sub(bottom).min(desired_height);

    (height >= 3).then_some(Rect::new(
        input_area.x,
        input_area.bottom(),
        input_area.width,
        height,
    ))
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

/// Wraps one comment input while preserving explicit newlines and cursor row.
fn wrapped_comment_input(
    input: &crate::domain::input::InputState,
    show_cursor: bool,
    width: usize,
) -> (Vec<String>, usize) {
    let width = width.max(1);
    let characters = input.text().chars().collect::<Vec<_>>();
    let cursor = input.cursor.min(characters.len());
    let mut current_line = String::new();
    let mut current_width = 0;
    let mut cursor_row = 0;
    let mut lines = Vec::new();

    for character_index in 0..=characters.len() {
        if show_cursor && character_index == cursor {
            push_wrapped_comment_character(
                '|',
                true,
                width,
                &mut lines,
                &mut current_line,
                &mut current_width,
                &mut cursor_row,
            );
        }
        let Some(character) = characters.get(character_index).copied() else {
            break;
        };
        if character == '\n' {
            lines.push(std::mem::take(&mut current_line));
            current_width = 0;

            continue;
        }

        push_wrapped_comment_character(
            character,
            false,
            width,
            &mut lines,
            &mut current_line,
            &mut current_width,
            &mut cursor_row,
        );
    }
    lines.push(current_line);

    (lines, cursor_row)
}

/// Appends one character, starting a new hard-wrapped row when required.
fn push_wrapped_comment_character(
    character: char,
    is_cursor: bool,
    width: usize,
    lines: &mut Vec<String>,
    current_line: &mut String,
    current_width: &mut usize,
    cursor_row: &mut usize,
) {
    let character_width = character.width().unwrap_or_default();
    if *current_width > 0 && current_width.saturating_add(character_width) > width {
        lines.push(std::mem::take(current_line));
        *current_width = 0;
    }
    if is_cursor {
        *cursor_row = lines.len();
    }
    current_line.push(character);
    *current_width = current_width.saturating_add(character_width);
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
#[path = "diff_test.rs"]
mod tests;
