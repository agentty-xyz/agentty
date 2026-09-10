use std::collections::BTreeMap;
use std::sync::Arc;

use ag_tui_text::text_util;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};

use crate::ui::diff_util::{DiffLine, DiffLineKind, FileTreeItem, diff_header_paths};
use crate::ui::{Component, style};

const DIFF_GIT_FILE_HEADER_PREFIX: &str = "diff --git";

const DIFF_GIT_FALLBACK_PREFIX: &str = "diff --git ";

const FILE_EXPLORER_HORIZONTAL_BORDER_WIDTH: u16 = 2;

const FILE_EXPLORER_TITLE: &str = " Files ";

const LOADING_LABEL: &str = "Loading...";

const NO_FILES_LABEL: &str = "No files";

const PATH_SEGMENT_SEPARATOR: char = '/';

const FOLDER_SUFFIX: &str = "/";

const TREE_BRANCH_MIDDLE: &str = "├ ";

const TREE_BRANCH_LAST: &str = "└ ";

const TREE_PREFIX_CONTINUATION: &str = "│ ";

const TREE_PREFIX_SPACER: &str = "  ";

const RENAME_ORIGIN_PREFIX: &str = " <- ";

const ROOT_TREE_PREFIX: &str = "";

/// A file entry in the tree along with optional rename origin metadata.
#[derive(Clone)]
struct FileLeaf {
    name: String,
    rename_from: Option<String>,
}

/// Parsed and normalized file path details extracted from a diff header.
#[derive(Debug)]
struct ParsedPath {
    path_segments: Vec<String>,
    rename_from: Option<String>,
}

/// Tree node containing nested folders and files for a diff file list.
#[derive(Default)]
struct FileTreeNode {
    files: Vec<FileLeaf>,
    folders: BTreeMap<String, FileTreeNode>,
}

impl FileTreeNode {
    /// Inserts a parsed file path into the tree, creating parent folders as
    /// needed.
    fn insert(&mut self, parsed_path: ParsedPath) {
        let ParsedPath {
            path_segments,
            rename_from,
        } = parsed_path;
        let Some((file_name, folder_segments)) = path_segments.split_last() else {
            return;
        };

        let mut current_node = self;
        for folder_name in folder_segments {
            current_node = current_node.folders.entry(folder_name.clone()).or_default();
        }

        current_node.files.push(FileLeaf {
            name: file_name.clone(),
            rename_from,
        });
    }

    /// Sorts files and all descendants to keep rendering deterministic.
    fn sort_recursive(&mut self) {
        self.files.sort_by(|left, right| left.name.cmp(&right.name));

        for child in self.folders.values_mut() {
            child.sort_recursive();
        }
    }
}

/// Diff file explorer panel rendering the changed file list.
pub struct FileExplorer {
    file_list_lines: Arc<[Line<'static>]>,
    is_focused: bool,
    preserved_suffix_span_count: usize,
    selected_index: usize,
}

impl FileExplorer {
    /// Creates a new file explorer component from parsed diff lines.
    pub fn new(parsed_lines: &[DiffLine<'_>]) -> Self {
        let (file_list_lines, _) = Self::file_tree(parsed_lines);

        Self {
            file_list_lines: Arc::from(file_list_lines),
            is_focused: true,
            preserved_suffix_span_count: 0,
            selected_index: 0,
        }
    }

    /// Creates a non-selectable loading placeholder for the diff sidebar.
    pub(crate) fn loading() -> Self {
        Self {
            file_list_lines: Arc::from([Line::from(Span::styled(
                LOADING_LABEL,
                Style::default().fg(style::palette::text_subtle()),
            ))]),
            is_focused: false,
            preserved_suffix_span_count: 0,
            selected_index: 0,
        }
    }

    /// Creates a file explorer component from cached rendered tree lines while
    /// preserving the requested number of trailing spans when labels overflow.
    pub(crate) fn from_cached_lines(
        file_list_lines: Arc<[Line<'static>]>,
        preserved_suffix_span_count: usize,
    ) -> Self {
        Self {
            file_list_lines,
            is_focused: true,
            preserved_suffix_span_count,
            selected_index: 0,
        }
    }

    /// Sets the selected item index in the file tree.
    #[must_use]
    pub fn selected_index(mut self, index: usize) -> Self {
        self.selected_index = index;
        self
    }

    /// Controls whether the selected file row receives focus highlighting.
    #[must_use]
    pub fn focused(mut self, is_focused: bool) -> Self {
        self.is_focused = is_focused;

        self
    }

    /// Returns the next selected index for a file list of `item_count` items.
    ///
    /// Selection wraps to the first item when moving forward from the last
    /// item. When `item_count` is zero, `current_index` is returned unchanged.
    pub fn next_selected_index(current_index: usize, item_count: usize) -> usize {
        if item_count == 0 {
            return current_index;
        }

        let normalized_index = Self::normalize_selected_index(current_index, item_count);

        (normalized_index + 1) % item_count
    }

    /// Returns the previous selected index for a file list of `item_count`
    /// items.
    ///
    /// Selection wraps to the last item when moving backward from the first
    /// item. When `item_count` is zero, `current_index` is returned unchanged.
    pub fn previous_selected_index(current_index: usize, item_count: usize) -> usize {
        if item_count == 0 {
            return current_index;
        }

        let normalized_index = Self::normalize_selected_index(current_index, item_count);

        if normalized_index == 0 {
            item_count - 1
        } else {
            normalized_index - 1
        }
    }

    /// Returns the selected item's row inside the bordered list viewport.
    ///
    /// The explorer creates a fresh [`ListState`] for every render. Ratatui
    /// therefore starts at offset zero and, when necessary, scrolls just far
    /// enough to keep the selected one-line item visible at the bottom.
    pub(crate) fn selected_visual_row(
        selected_index: usize,
        item_count: usize,
        area: Rect,
    ) -> Option<u16> {
        if item_count == 0 {
            return None;
        }
        let viewport_height = Block::default().borders(Borders::ALL).inner(area).height;
        if viewport_height == 0 {
            return None;
        }
        let selected_index = Self::normalize_selected_index(selected_index, item_count);
        let last_visual_row = usize::from(viewport_height.saturating_sub(1));

        u16::try_from(selected_index.min(last_visual_row)).ok()
    }

    /// Returns the number of items (files and folders) in the explorer list.
    pub fn count_items(parsed_lines: &[DiffLine<'_>]) -> usize {
        let (lines, _) = Self::file_tree(parsed_lines);

        lines.len()
    }

    /// Returns the [`FileTreeItem`] list for the given parsed diff lines.
    ///
    /// Each entry corresponds one-to-one to a rendered tree line so the
    /// selected index can be used to look up the matching item.
    pub fn file_tree_items(parsed_lines: &[DiffLine<'_>]) -> Vec<FileTreeItem> {
        let (_, items) = Self::file_tree(parsed_lines);

        items
    }

    /// Builds the rendered file tree lines and matching selection items for
    /// one parsed diff snapshot.
    pub(crate) fn file_tree(
        parsed_lines: &[DiffLine<'_>],
    ) -> (Vec<Line<'static>>, Vec<FileTreeItem>) {
        Self::build_tree(parsed_lines)
    }

    /// Builds the tree display lines and parallel [`FileTreeItem`] list from
    /// parsed diff headers.
    fn build_tree(parsed_lines: &[DiffLine<'_>]) -> (Vec<Line<'static>>, Vec<FileTreeItem>) {
        let mut file_tree = FileTreeNode::default();

        for diff_line in parsed_lines {
            if diff_line.kind != DiffLineKind::FileHeader
                || !diff_line.content.starts_with(DIFF_GIT_FILE_HEADER_PREFIX)
            {
                continue;
            }

            if let Some(parsed_path) = Self::parse_path(diff_line.content) {
                file_tree.insert(parsed_path);
            }
        }

        let mut file_list_lines = Vec::new();
        let mut items = Vec::new();
        file_tree.sort_recursive();
        Self::append_tree_lines(
            &file_tree,
            ROOT_TREE_PREFIX,
            ROOT_TREE_PREFIX,
            &mut file_list_lines,
            &mut items,
        );

        if file_list_lines.is_empty() {
            file_list_lines.push(Line::from(Span::styled(
                NO_FILES_LABEL,
                Style::default().fg(style::palette::text_subtle()),
            )));
        }

        (file_list_lines, items)
    }

    /// Clamps `current_index` to a valid list index for `item_count` items.
    fn normalize_selected_index(current_index: usize, item_count: usize) -> usize {
        current_index.min(item_count.saturating_sub(1))
    }

    /// Parses a diff header into a normalized path representation for tree
    /// insertion.
    fn parse_path(file_header_line: &str) -> Option<ParsedPath> {
        if let Some((old_path, new_path)) = diff_header_paths(file_header_line) {
            let path_segments = Self::split_path_segments(&new_path);
            if path_segments.is_empty() {
                return None;
            }

            let rename_from = (old_path != new_path).then_some(old_path);

            return Some(ParsedPath {
                path_segments,
                rename_from,
            });
        }

        Some(ParsedPath {
            path_segments: vec![file_header_line.replace(DIFF_GIT_FALLBACK_PREFIX, "")],
            rename_from: None,
        })
    }

    /// Splits a repository-relative path into individual folder/file segments.
    fn split_path_segments(path: &str) -> Vec<String> {
        path.split(PATH_SEGMENT_SEPARATOR)
            .filter(|segment| !segment.is_empty())
            .map(ToString::to_string)
            .collect()
    }

    /// Appends a depth-first textual tree representation for the node and its
    /// children, while building a parallel [`FileTreeItem`] list.
    fn append_tree_lines(
        node: &FileTreeNode,
        prefix: &str,
        path_prefix: &str,
        lines: &mut Vec<Line<'static>>,
        items: &mut Vec<FileTreeItem>,
    ) {
        let total_children = node.folders.len() + node.files.len();
        let mut child_index = 0;

        for (folder_name, folder_node) in &node.folders {
            child_index += 1;
            let is_last_child = child_index == total_children;
            let is_root = path_prefix.is_empty();
            let branch_prefix = Self::tree_branch_prefix(is_root, is_last_child);
            let (folder_label, folder_path, compacted_node) =
                Self::compact_folder_chain(folder_name, folder_node, path_prefix);
            let line_text = format!("{prefix}{branch_prefix}{folder_label}{FOLDER_SUFFIX}");

            lines.push(Line::from(Span::styled(
                line_text,
                Style::default().fg(style::palette::warning()),
            )));
            items.push(FileTreeItem::Folder(folder_path.clone()));

            let child_prefix = Self::child_tree_prefix(prefix, is_root, is_last_child);

            Self::append_tree_lines(compacted_node, &child_prefix, &folder_path, lines, items);
        }

        for file in &node.files {
            child_index += 1;
            let is_last_child = child_index == total_children;
            let branch_prefix = Self::tree_branch_prefix(path_prefix.is_empty(), is_last_child);
            let file_name = format!("{prefix}{branch_prefix}{}", file.name);
            let file_path = format!("{path_prefix}{}", file.name);
            let mut spans = vec![Span::styled(
                file_name,
                Style::default().fg(style::palette::accent()),
            )];

            if let Some(rename_from) = &file.rename_from {
                spans.push(Span::styled(
                    format!("{RENAME_ORIGIN_PREFIX}{rename_from}"),
                    Style::default().fg(style::palette::text_subtle()),
                ));
            }

            lines.push(Line::from(spans));
            items.push(FileTreeItem::File(file_path));
        }
    }

    /// Returns the connector shown before one folder or file row.
    fn tree_branch_prefix(is_root: bool, is_last_child: bool) -> &'static str {
        if is_root {
            return ROOT_TREE_PREFIX;
        }
        if is_last_child {
            return TREE_BRANCH_LAST;
        }

        TREE_BRANCH_MIDDLE
    }

    /// Returns the indentation prefix inherited by one folder's children.
    fn child_tree_prefix(prefix: &str, is_root: bool, is_last_child: bool) -> String {
        if is_root {
            return ROOT_TREE_PREFIX.to_string();
        }
        if is_last_child {
            return format!("{prefix}{TREE_PREFIX_SPACER}");
        }

        format!("{prefix}{TREE_PREFIX_CONTINUATION}")
    }

    /// Collapses an uninterrupted folder-only chain into one display label.
    fn compact_folder_chain<'node>(
        folder_name: &str,
        folder_node: &'node FileTreeNode,
        path_prefix: &str,
    ) -> (String, String, &'node FileTreeNode) {
        let folder_label = folder_name.to_string();
        let folder_path = format!("{path_prefix}{folder_name}/");
        if !folder_node.files.is_empty() || folder_node.folders.len() != 1 {
            return (folder_label, folder_path, folder_node);
        }

        folder_node.folders.iter().fold(
            (folder_label, folder_path, folder_node),
            |(mut folder_label, folder_path, _), (child_name, child_node)| {
                let (child_label, child_path, compacted_node) =
                    Self::compact_folder_chain(child_name, child_node, &folder_path);
                folder_label.push(PATH_SEGMENT_SEPARATOR);
                folder_label.push_str(&child_label);

                (folder_label, child_path, compacted_node)
            },
        )
    }

    /// Right-aligns a preserved suffix, truncating the file-tree label first
    /// when both cannot fit in the available width.
    fn line_for_width(&self, line: &Line<'static>, max_width: usize) -> Line<'static> {
        let suffix_start = line
            .spans
            .len()
            .saturating_sub(self.preserved_suffix_span_count);
        if self.preserved_suffix_span_count == 0 || suffix_start == 0 {
            return line.clone();
        }

        let suffix_spans = line.spans[suffix_start..].to_vec();
        let suffix_width = suffix_spans.iter().map(Span::width).sum::<usize>();
        let available_prefix_width = max_width.saturating_sub(suffix_width);
        let prefix_spans = line.spans[..suffix_start].to_vec();
        let prefix_width = prefix_spans.iter().map(Span::width).sum::<usize>();
        let mut spans = if prefix_width > available_prefix_width {
            text_util::truncate_spans_with_ellipsis(prefix_spans, available_prefix_width)
        } else {
            prefix_spans
        };
        let rendered_prefix_width = spans.iter().map(Span::width).sum::<usize>();
        let padding_width = available_prefix_width.saturating_sub(rendered_prefix_width);
        spans.push(Span::raw(" ".repeat(padding_width)));
        spans.extend(suffix_spans);

        Line::from(spans)
    }
}

impl Component for FileExplorer {
    fn render(&self, f: &mut Frame, area: Rect) {
        let content_width = usize::from(
            area.width
                .saturating_sub(FILE_EXPLORER_HORIZONTAL_BORDER_WIDTH),
        );
        let items: Vec<ListItem> = self
            .file_list_lines
            .iter()
            .map(|line| self.line_for_width(line, content_width))
            .map(ListItem::new)
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Span::styled(
                        FILE_EXPLORER_TITLE,
                        Style::default().fg(style::palette::accent()),
                    ))
                    .border_style(style::border_style()),
            )
            .highlight_style(Style::default().bg(style::palette::surface_selection()));

        let mut state = ListState::default();
        if self.is_focused {
            state.select(Some(self.selected_index));
        }

        f.render_stateful_widget(list, area, &mut state);
    }
}

#[cfg(test)]
#[path = "file_explorer_test.rs"]
mod tests;
