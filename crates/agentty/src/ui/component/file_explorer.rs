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

/// Diff file explorer panel rendering the changed file list.
pub struct FileExplorer {
    file_list_lines: Arc<[Line<'static>]>,
    is_focused: bool,
    preserved_suffix_span_count: usize,
    selected_index: usize,
}

/// A file entry in the tree along with optional rename origin metadata.
#[derive(Clone)]
struct FileLeaf {
    name: String,
    rename_from: Option<String>,
}

/// Tree node containing nested folders and files for a diff file list.
#[derive(Default)]
struct FileTreeNode {
    files: Vec<FileLeaf>,
    folders: BTreeMap<String, FileTreeNode>,
}

/// Parsed and normalized file path details extracted from a diff header.
#[derive(Debug)]
struct ParsedPath {
    path_segments: Vec<String>,
    rename_from: Option<String>,
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
mod tests {
    use super::*;
    use crate::domain::theme::ColorTheme;

    #[test]
    fn test_render_uses_palette_border_for_file_explorer() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let parsed_lines = vec![DiffLine {
            kind: DiffLineKind::FileHeader,
            old_line: None,
            new_line: None,
            content: DIFF_SAME_PATH_HEADER,
        }];
        let backend = ratatui::backend::TestBackend::new(40, 10);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                FileExplorer::new(&parsed_lines).render(frame, frame.area());
            })
            .expect("failed to draw file explorer");

        // Assert
        let buffer = terminal.backend().buffer();
        let border_cell = &buffer.content()[0];
        assert_eq!(border_cell.symbol(), "┌");
        assert_eq!(border_cell.fg, style::palette::border());
    }

    #[test]
    fn loading_renders_explicit_placeholder_without_empty_state() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(40, 10);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| FileExplorer::loading().render(frame, frame.area()))
            .expect("failed to draw loading file explorer");

        // Assert
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(text.contains(LOADING_LABEL));
        assert!(!text.contains(NO_FILES_LABEL));
    }

    #[test]
    fn test_render_right_aligns_preserved_suffix_and_truncates_long_labels() {
        // Arrange
        let change_totals = || {
            [
                Span::raw(" "),
                Span::styled("+12", Style::default().fg(style::palette::success())),
                Span::styled("/", Style::default().fg(style::palette::text_muted())),
                Span::styled("-3", Style::default().fg(style::palette::danger())),
            ]
        };
        let lines: Arc<[Line<'static>]> = Arc::from([
            Line::from(
                [Span::styled(
                    "└ main.rs",
                    Style::default().fg(style::palette::accent()),
                )]
                .into_iter()
                .chain(change_totals())
                .collect::<Vec<_>>(),
            ),
            Line::from(
                [Span::styled(
                    "└ path/to/longer/main.rs",
                    Style::default().fg(style::palette::accent()),
                )]
                .into_iter()
                .chain(change_totals())
                .collect::<Vec<_>>(),
            ),
        ]);
        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                FileExplorer::from_cached_lines(lines.clone(), 4).render(frame, frame.area());
            })
            .expect("failed to draw file explorer");

        // Assert
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(text.contains("└ main.rs   +12/-3"));
        assert!(text.contains("└ path/t... +12/-3"));
    }

    const DIFF_SAME_PATH_HEADER: &str = "diff --git a/src/main.rs b/src/main.rs";
    const DIFF_RENAME_HEADER: &str = "diff --git a/src/old.rs b/src/new.rs";
    const DIFF_NONSTANDARD_HEADER: &str = "diff --git old/path new/path";
    const DIFF_README_HEADER: &str = "diff --git a/README.md b/README.md";
    const DIFF_NESTED_HEADER: &str =
        "diff --git a/src/ui/component/file_explorer.rs b/src/ui/component/file_explorer.rs";
    const DIFF_SIBLING_FOLDER_HEADER: &str =
        "diff --git a/src/domain/session.rs b/src/domain/session.rs";
    const DIFF_QUOTED_MARKDOWN_HEADER: &str = concat!(
        "diff --git \"a/docs/\\346\\227\\245\\346\\234\\254.md\" ",
        "\"b/docs/\\346\\227\\245\\346\\234\\254.md\"",
    );
    const EXPECTED_SRC_FOLDER_LINE: &str = "src/";
    const EXPECTED_MAIN_FILE_LINE: &str = "└ main.rs";
    const EXPECTED_NEW_FILE_LINE: &str = "└ new.rs";
    const EXPECTED_RENAME_LINE: &str = " <- src/old.rs";
    const EXPECTED_NONSTANDARD_LINE: &str = "old/path new/path";
    const EXPECTED_NESTED_TREE_LINES: [&str; 5] = [
        "src/",
        "├ ui/component/",
        "│ └ file_explorer.rs",
        "└ main.rs",
        "README.md",
    ];
    const UNCHANGED_DIFF_LINE: &str = " unchanged";

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.to_string())
            .collect()
    }

    #[test]
    fn test_selected_visual_row_matches_fresh_list_state_scrolling() {
        // Arrange
        let tall_area = Rect::new(0, 0, 20, 12);
        let short_area = Rect::new(0, 0, 20, 5);
        let flat_area = Rect::new(0, 0, 20, 2);

        // Act
        let visible_row = FileExplorer::selected_visual_row(7, 10, tall_area);
        let scrolled_row = FileExplorer::selected_visual_row(7, 10, short_area);
        let clamped_row = FileExplorer::selected_visual_row(usize::MAX, 2, tall_area);
        let empty_row = FileExplorer::selected_visual_row(0, 0, tall_area);
        let flat_row = FileExplorer::selected_visual_row(0, 1, flat_area);

        // Assert
        assert_eq!(visible_row, Some(7));
        assert_eq!(scrolled_row, Some(2));
        assert_eq!(clamped_row, Some(1));
        assert_eq!(empty_row, None);
        assert_eq!(flat_row, None);
    }

    #[test]
    fn test_file_list_lines_with_same_path() {
        // Arrange
        let parsed_lines = vec![DiffLine {
            kind: DiffLineKind::FileHeader,
            old_line: None,
            new_line: None,
            content: DIFF_SAME_PATH_HEADER,
        }];

        // Act
        let lines = FileExplorer::build_tree(&parsed_lines).0;

        // Assert
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[0].content, EXPECTED_SRC_FOLDER_LINE);
        assert_eq!(lines[1].spans[0].content, EXPECTED_MAIN_FILE_LINE);
    }

    #[test]
    fn test_next_selected_index_wraps_from_last_to_first() {
        // Arrange
        let current_index = 1;
        let item_count = 2;

        // Act
        let next_index = FileExplorer::next_selected_index(current_index, item_count);

        // Assert
        assert_eq!(next_index, 0);
    }

    #[test]
    fn test_previous_selected_index_wraps_from_first_to_last() {
        // Arrange
        let current_index = 0;
        let item_count = 2;

        // Act
        let previous_index = FileExplorer::previous_selected_index(current_index, item_count);

        // Assert
        assert_eq!(previous_index, 1);
    }

    #[test]
    fn test_file_list_lines_with_rename() {
        // Arrange
        let parsed_lines = vec![DiffLine {
            kind: DiffLineKind::FileHeader,
            old_line: None,
            new_line: None,
            content: DIFF_RENAME_HEADER,
        }];

        // Act
        let lines = FileExplorer::build_tree(&parsed_lines).0;

        // Assert
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[0].content, EXPECTED_SRC_FOLDER_LINE);
        assert_eq!(lines[1].spans[0].content, EXPECTED_NEW_FILE_LINE);
        assert_eq!(lines[1].spans[1].content, EXPECTED_RENAME_LINE);
    }

    #[test]
    fn test_file_list_lines_with_nonstandard_header() {
        // Arrange
        let parsed_lines = vec![DiffLine {
            kind: DiffLineKind::FileHeader,
            old_line: None,
            new_line: None,
            content: DIFF_NONSTANDARD_HEADER,
        }];

        // Act
        let lines = FileExplorer::build_tree(&parsed_lines).0;

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content, EXPECTED_NONSTANDARD_LINE);
    }

    #[test]
    fn test_file_list_lines_with_nested_structure() {
        // Arrange
        let parsed_lines = vec![
            DiffLine {
                kind: DiffLineKind::FileHeader,
                old_line: None,
                new_line: None,
                content: DIFF_SAME_PATH_HEADER,
            },
            DiffLine {
                kind: DiffLineKind::FileHeader,
                old_line: None,
                new_line: None,
                content: DIFF_NESTED_HEADER,
            },
            DiffLine {
                kind: DiffLineKind::FileHeader,
                old_line: None,
                new_line: None,
                content: DIFF_README_HEADER,
            },
        ];

        // Act
        let lines = FileExplorer::build_tree(&parsed_lines).0;

        // Assert
        let line_text: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(
            line_text,
            EXPECTED_NESTED_TREE_LINES
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_file_list_lines_compact_single_child_folder_chain() {
        // Arrange
        let parsed_lines = vec![DiffLine {
            kind: DiffLineKind::FileHeader,
            old_line: None,
            new_line: None,
            content: DIFF_NESTED_HEADER,
        }];

        // Act
        let (lines, items) = FileExplorer::build_tree(&parsed_lines);

        // Assert
        assert_eq!(
            lines.iter().map(line_text).collect::<Vec<_>>(),
            ["src/ui/component/", "└ file_explorer.rs"]
        );
        assert_eq!(
            items,
            [
                FileTreeItem::Folder("src/ui/component/".to_string()),
                FileTreeItem::File("src/ui/component/file_explorer.rs".to_string()),
            ]
        );
    }

    #[test]
    fn test_file_list_lines_render_last_sibling_folder_branch() {
        // Arrange
        let parsed_lines = vec![
            DiffLine {
                kind: DiffLineKind::FileHeader,
                old_line: None,
                new_line: None,
                content: DIFF_SIBLING_FOLDER_HEADER,
            },
            DiffLine {
                kind: DiffLineKind::FileHeader,
                old_line: None,
                new_line: None,
                content: DIFF_NESTED_HEADER,
            },
        ];

        // Act
        let lines = FileExplorer::build_tree(&parsed_lines).0;

        // Assert
        assert_eq!(
            lines.iter().map(line_text).collect::<Vec<_>>(),
            [
                "src/",
                "├ domain/",
                "│ └ session.rs",
                "└ ui/component/",
                "  └ file_explorer.rs",
            ]
        );
    }

    #[test]
    fn test_file_list_lines_with_no_files() {
        // Arrange
        let parsed_lines = vec![DiffLine {
            kind: DiffLineKind::Context,
            old_line: Some(1),
            new_line: Some(1),
            content: UNCHANGED_DIFF_LINE,
        }];

        // Act
        let lines = FileExplorer::build_tree(&parsed_lines).0;

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content, NO_FILES_LABEL);
    }

    #[test]
    fn test_file_tree_items_returns_folders_and_files() {
        // Arrange
        let parsed_lines = vec![DiffLine {
            kind: DiffLineKind::FileHeader,
            old_line: None,
            new_line: None,
            content: DIFF_SAME_PATH_HEADER,
        }];

        // Act
        let items = FileExplorer::file_tree_items(&parsed_lines);

        // Assert
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], FileTreeItem::Folder("src/".to_string()));
        assert_eq!(items[1], FileTreeItem::File("src/main.rs".to_string()));
    }

    #[test]
    fn test_file_tree_items_nested_structure() {
        // Arrange
        let parsed_lines = vec![
            DiffLine {
                kind: DiffLineKind::FileHeader,
                old_line: None,
                new_line: None,
                content: DIFF_SAME_PATH_HEADER,
            },
            DiffLine {
                kind: DiffLineKind::FileHeader,
                old_line: None,
                new_line: None,
                content: DIFF_NESTED_HEADER,
            },
            DiffLine {
                kind: DiffLineKind::FileHeader,
                old_line: None,
                new_line: None,
                content: DIFF_README_HEADER,
            },
        ];

        // Act
        let items = FileExplorer::file_tree_items(&parsed_lines);

        // Assert
        assert_eq!(
            items,
            vec![
                FileTreeItem::Folder("src/".to_string()),
                FileTreeItem::Folder("src/ui/component/".to_string()),
                FileTreeItem::File("src/ui/component/file_explorer.rs".to_string()),
                FileTreeItem::File("src/main.rs".to_string()),
                FileTreeItem::File("README.md".to_string()),
            ]
        );
    }

    #[test]
    fn test_file_tree_items_with_rename() {
        // Arrange
        let parsed_lines = vec![DiffLine {
            kind: DiffLineKind::FileHeader,
            old_line: None,
            new_line: None,
            content: DIFF_RENAME_HEADER,
        }];

        // Act
        let items = FileExplorer::file_tree_items(&parsed_lines);

        // Assert
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], FileTreeItem::Folder("src/".to_string()));
        assert_eq!(items[1], FileTreeItem::File("src/new.rs".to_string()));
    }

    #[test]
    fn test_file_tree_items_decode_git_quoted_paths() {
        // Arrange
        let parsed_lines = vec![DiffLine {
            kind: DiffLineKind::FileHeader,
            old_line: None,
            new_line: None,
            content: DIFF_QUOTED_MARKDOWN_HEADER,
        }];

        // Act
        let items = FileExplorer::file_tree_items(&parsed_lines);

        // Assert
        assert_eq!(
            items,
            vec![
                FileTreeItem::Folder("docs/".to_string()),
                FileTreeItem::File("docs/日本.md".to_string()),
            ]
        );
    }

    #[test]
    fn test_file_tree_items_ignore_empty_new_path() {
        // Arrange
        let parsed_lines = vec![DiffLine {
            kind: DiffLineKind::FileHeader,
            old_line: None,
            new_line: None,
            content: "diff --git a/old.md b/",
        }];

        // Act
        let items = FileExplorer::file_tree_items(&parsed_lines);

        // Assert
        assert_eq!(items, [] as [crate::ui::diff_util::FileTreeItem; 0]);
    }
}
