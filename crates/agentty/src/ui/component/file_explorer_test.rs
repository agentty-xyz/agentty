use std::sync::Arc;

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{FileExplorer, LOADING_LABEL, NO_FILES_LABEL};
use crate::domain::theme::ColorTheme;
use crate::ui::diff_util::{DiffLine, DiffLineKind, FileTreeItem};
use crate::ui::render::Component;
use crate::ui::style;

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
