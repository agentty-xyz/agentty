use std::collections::BTreeMap;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};

use crate::ui::Component;
use crate::ui::util::{DiffLine, DiffLineKind};

const DIFF_GIT_FILE_HEADER_PREFIX: &str = "diff --git";
const DIFF_GIT_PATH_PREFIX: &str = "diff --git a/";
const DIFF_GIT_PATH_SEPARATOR: &str = " b/";
const DIFF_GIT_FALLBACK_PREFIX: &str = "diff --git ";
const FILE_EXPLORER_TITLE: &str = " Files ";
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
    file_list_lines: Vec<Line<'static>>,
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
    pub fn new(parsed_lines: &[DiffLine<'_>], selected_index: usize) -> Self {
        let file_list_lines = Self::file_list_lines(parsed_lines);

        Self {
            file_list_lines,
            selected_index,
        }
    }

    /// Returns the number of items (files and folders) in the explorer list.
    pub fn count_items(parsed_lines: &[DiffLine<'_>]) -> usize {
        Self::file_list_lines(parsed_lines).len()
    }

    /// Builds tree-rendered lines from parsed diff headers.
    fn file_list_lines(parsed_lines: &[DiffLine<'_>]) -> Vec<Line<'static>> {
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
        file_tree.sort_recursive();
        Self::append_tree_lines(&file_tree, ROOT_TREE_PREFIX, &mut file_list_lines);

        if file_list_lines.is_empty() {
            file_list_lines.push(Line::from(Span::styled(
                NO_FILES_LABEL,
                Style::default().fg(Color::DarkGray),
            )));
        }

        file_list_lines
    }

    /// Parses a diff header into a normalized path representation for tree
    /// insertion.
    fn parse_path(file_header_line: &str) -> Option<ParsedPath> {
        if let Some(stripped_header) = file_header_line.strip_prefix(DIFF_GIT_PATH_PREFIX) {
            if let Some((old_path, new_path)) = stripped_header.split_once(DIFF_GIT_PATH_SEPARATOR)
            {
                let path_segments = Self::split_path_segments(new_path);
                if path_segments.is_empty() {
                    return None;
                }

                let rename_from = if old_path == new_path {
                    None
                } else {
                    Some(old_path.to_string())
                };

                return Some(ParsedPath {
                    path_segments,
                    rename_from,
                });
            }

            return Some(ParsedPath {
                path_segments: vec![stripped_header.to_string()],
                rename_from: None,
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
    /// children.
    fn append_tree_lines(node: &FileTreeNode, prefix: &str, lines: &mut Vec<Line<'static>>) {
        let total_children = node.folders.len() + node.files.len();
        let mut child_index = 0;

        for (folder_name, folder_node) in &node.folders {
            child_index += 1;
            let is_last_child = child_index == total_children;
            let branch_prefix = if is_last_child {
                TREE_BRANCH_LAST
            } else {
                TREE_BRANCH_MIDDLE
            };
            let line_text = format!("{prefix}{branch_prefix}{folder_name}{FOLDER_SUFFIX}");

            lines.push(Line::from(Span::styled(
                line_text,
                Style::default().fg(Color::Yellow),
            )));

            let child_prefix = if is_last_child {
                format!("{prefix}{TREE_PREFIX_SPACER}")
            } else {
                format!("{prefix}{TREE_PREFIX_CONTINUATION}")
            };

            Self::append_tree_lines(folder_node, &child_prefix, lines);
        }

        for file in &node.files {
            child_index += 1;
            let is_last_child = child_index == total_children;
            let branch_prefix = if is_last_child {
                TREE_BRANCH_LAST
            } else {
                TREE_BRANCH_MIDDLE
            };
            let file_name = format!("{prefix}{branch_prefix}{}", file.name);
            let mut spans = vec![Span::styled(file_name, Style::default().fg(Color::Cyan))];

            if let Some(rename_from) = &file.rename_from {
                spans.push(Span::styled(
                    format!("{RENAME_ORIGIN_PREFIX}{rename_from}"),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            lines.push(Line::from(spans));
        }
    }
}

impl Component for FileExplorer {
    fn render(&self, f: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .file_list_lines
            .iter()
            .cloned()
            .map(ListItem::new)
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(Span::styled(
                FILE_EXPLORER_TITLE,
                Style::default().fg(Color::Cyan),
            )))
            .highlight_style(Style::default().bg(Color::DarkGray));

        let mut state = ListState::default();
        state.select(Some(self.selected_index));

        f.render_stateful_widget(list, area, &mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF_SAME_PATH_HEADER: &str = "diff --git a/src/main.rs b/src/main.rs";
    const DIFF_RENAME_HEADER: &str = "diff --git a/src/old.rs b/src/new.rs";
    const DIFF_NONSTANDARD_HEADER: &str = "diff --git old/path new/path";
    const DIFF_README_HEADER: &str = "diff --git a/README.md b/README.md";
    const DIFF_NESTED_HEADER: &str =
        "diff --git a/src/ui/components/file_explorer.rs b/src/ui/components/file_explorer.rs";
    const EXPECTED_SRC_FOLDER_LINE: &str = "└ src/";
    const EXPECTED_MAIN_FILE_LINE: &str = "  └ main.rs";
    const EXPECTED_NEW_FILE_LINE: &str = "  └ new.rs";
    const EXPECTED_RENAME_LINE: &str = " <- src/old.rs";
    const EXPECTED_NONSTANDARD_LINE: &str = "└ old/path new/path";
    const EXPECTED_NESTED_TREE_LINES: [&str; 6] = [
        "├ src/",
        "│ ├ ui/",
        "│ │ └ components/",
        "│ │   └ file_explorer.rs",
        "│ └ main.rs",
        "└ README.md",
    ];
    const UNCHANGED_DIFF_LINE: &str = " unchanged";

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
        let lines = FileExplorer::file_list_lines(&parsed_lines);

        // Assert
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[0].content, EXPECTED_SRC_FOLDER_LINE);
        assert_eq!(lines[1].spans[0].content, EXPECTED_MAIN_FILE_LINE);
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
        let lines = FileExplorer::file_list_lines(&parsed_lines);

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
        let lines = FileExplorer::file_list_lines(&parsed_lines);

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
        let lines = FileExplorer::file_list_lines(&parsed_lines);

        // Assert
        let line_text: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect();
        assert_eq!(
            line_text,
            EXPECTED_NESTED_TREE_LINES
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
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
        let lines = FileExplorer::file_list_lines(&parsed_lines);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content, NO_FILES_LABEL);
    }
}
