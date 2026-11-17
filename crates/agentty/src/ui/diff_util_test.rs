//! Unified-diff header and hunk parsing, filtering, wrapping, and diff page
//! layout unit tests.

use ratatui::layout::Rect;

use super::{
    DiffLine, DiffLineKind, DiffRenderLayout, FileTreeItem, diff_header_new_path,
    diff_header_paths, diff_line_gutter_width, diff_page_areas, diff_view_max_scroll_offset,
    filter_diff_lines, max_diff_line_number, parse_diff_lines, parse_git_path_token,
    parse_hunk_header, rendered_diff_line_count, wrap_diff_content,
};

const DIFF_MAIN_HEADER: &str = "diff --git a/src/main.rs b/src/main.rs";
const DIFF_NESTED_HEADER: &str =
    "diff --git a/src/ui/component/file_explorer.rs b/src/ui/component/file_explorer.rs";
const DIFF_README_HEADER: &str = "diff --git a/README.md b/README.md";

#[test]
fn test_diff_header_paths_decodes_git_quoted_non_ascii_paths() {
    // Arrange
    let header = concat!(
        "diff --git \"a/docs/\\346\\227\\245\\346\\234\\254.md\" ",
        "\"b/docs/\\346\\227\\245\\346\\234\\254.md\"",
    );

    // Act
    let paths = diff_header_paths(header);
    let new_path = diff_header_new_path(header);

    // Assert
    assert_eq!(
        paths,
        Some(("docs/日本.md".to_string(), "docs/日本.md".to_string()))
    );
    assert_eq!(new_path, Some("docs/日本.md".to_string()));
}

#[test]
fn test_diff_header_paths_decodes_spaces_and_c_escapes() {
    // Arrange
    let spaced_header = "diff --git \"a/docs/old file.md\" \"b/docs/new file.md\"";
    let escape_cases = [
        (r#""a/\a" tail"#, "a/\x07"),
        (r#""a/\b" tail"#, "a/\x08"),
        (r#""a/\t" tail"#, "a/\t"),
        (r#""a/\n" tail"#, "a/\n"),
        (r#""a/\v" tail"#, "a/\x0b"),
        (r#""a/\f" tail"#, "a/\x0c"),
        (r#""a/\r" tail"#, "a/\r"),
        (r#""a/\\" tail"#, "a/\\"),
        (r#""a/\"" tail"#, "a/\""),
        (r#""a/\7x" tail"#, "a/\x07x"),
    ];

    // Act
    let spaced_paths = diff_header_paths(spaced_header);
    let decoded_escapes = escape_cases
        .iter()
        .map(|(encoded, _)| parse_git_path_token(encoded))
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        spaced_paths,
        Some((
            "docs/old file.md".to_string(),
            "docs/new file.md".to_string(),
        ))
    );
    for ((_, expected), decoded) in escape_cases.iter().zip(decoded_escapes) {
        assert_eq!(decoded, Some(((*expected).to_string(), " tail")));
    }
}

#[test]
fn test_git_path_token_and_header_reject_malformed_input() {
    // Arrange
    let malformed_tokens = [
        "",
        r#""unterminated"#,
        r#""a/\q""#,
        r#""a/\"#,
        r#""a/\377""#,
    ];
    let malformed_headers = [
        "not a diff header",
        "diff --git a/only-one-path",
        "diff --git a/old.md b/new.md trailing",
        "diff --git old.md b/new.md",
        "diff --git a/old.md new.md",
        "diff --git \"a/old.md\"\"b/new.md\"",
    ];

    // Act
    let unquoted = parse_git_path_token("a/old.md b/new.md");
    let rejected_tokens = malformed_tokens
        .iter()
        .map(|token| parse_git_path_token(token))
        .collect::<Vec<_>>();
    let rejected_headers = malformed_headers
        .iter()
        .map(|header| diff_header_paths(header))
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(unquoted, Some(("a/old.md".to_string(), " b/new.md")));
    assert!(rejected_tokens.iter().all(Option::is_none));
    assert!(rejected_headers.iter().all(Option::is_none));
}

#[test]
fn test_parse_hunk_header_basic() {
    // Arrange
    let line = "@@ -10,5 +20,7 @@";

    // Act
    let result = parse_hunk_header(line);

    // Assert
    assert_eq!(result, Some((10, 5, 20, 7)));
}

#[test]
fn test_parse_hunk_header_no_count() {
    // Arrange
    let line = "@@ -1 +1 @@";

    // Act
    let result = parse_hunk_header(line);

    // Assert
    assert_eq!(result, Some((1, 1, 1, 1)));
}

#[test]
fn test_parse_hunk_header_with_context() {
    // Arrange
    let line = "@@ -100,3 +200,4 @@ fn main() {";

    // Act
    let result = parse_hunk_header(line);

    // Assert
    assert_eq!(result, Some((100, 3, 200, 4)));
}

#[test]
fn test_parse_hunk_header_invalid() {
    // Arrange & Act & Assert
    assert_eq!(parse_hunk_header("not a hunk"), None);
    assert_eq!(parse_hunk_header("@@@ invalid @@@"), None);
}

#[test]
fn test_parse_diff_lines_full() {
    // Arrange
    let diff = "\
diff --git a/file.rs b/file.rs
index abc..def 100644
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,4 @@
 line1
+added
 line2
-removed";

    // Act
    let lines = parse_diff_lines(diff);

    // Assert
    assert_eq!(lines.len(), 9);

    assert_eq!(lines[0].kind, DiffLineKind::FileHeader);
    assert_eq!(lines[0].content, "diff --git a/file.rs b/file.rs");
    assert_eq!(lines[0].old_line, None);

    assert_eq!(lines[4].kind, DiffLineKind::HunkHeader);
    assert_eq!(lines[4].old_line, None);

    assert_eq!(lines[5].kind, DiffLineKind::Context);
    assert_eq!(lines[5].content, "line1");
    assert_eq!(lines[5].old_line, Some(1));
    assert_eq!(lines[5].new_line, Some(1));

    assert_eq!(lines[6].kind, DiffLineKind::Addition);
    assert_eq!(lines[6].content, "added");
    assert_eq!(lines[6].old_line, None);
    assert_eq!(lines[6].new_line, Some(2));

    assert_eq!(lines[7].kind, DiffLineKind::Context);
    assert_eq!(lines[7].content, "line2");
    assert_eq!(lines[7].old_line, Some(2));
    assert_eq!(lines[7].new_line, Some(3));

    assert_eq!(lines[8].kind, DiffLineKind::Deletion);
    assert_eq!(lines[8].content, "removed");
    assert_eq!(lines[8].old_line, Some(3));
    assert_eq!(lines[8].new_line, None);
}

#[test]
fn test_parse_diff_lines_does_not_count_no_newline_marker() {
    // Arrange
    let diff = concat!(
        "diff --git a/file.rs b/file.rs\n",
        "@@ -1 +1 @@\n",
        "-old\n",
        "\\ No newline at end of file\n",
        "+new\n",
    );

    // Act
    let lines = parse_diff_lines(diff);

    // Assert
    assert_eq!(lines[3].content, r"\ No newline at end of file");
    assert_eq!(lines[3].old_line, None);
    assert_eq!(lines[3].new_line, None);
    assert_eq!(lines[4].new_line, Some(1));
}

#[test]
fn test_parse_diff_lines_empty() {
    // Arrange
    let diff = "";

    // Act
    let lines = parse_diff_lines(diff);

    // Assert
    assert_eq!(lines.len(), 0);
}

#[test]
fn test_max_diff_line_number() {
    // Arrange
    let diff = "\
@@ -95,3 +100,4 @@
 context
+added
 context2
-removed";
    let lines = parse_diff_lines(diff);

    // Act
    let max_num = max_diff_line_number(&lines);

    // Assert
    assert_eq!(max_num, 102);
}

#[test]
fn test_max_diff_line_number_empty() {
    // Arrange
    let lines: Vec<DiffLine<'_>> = Vec::new();

    // Act
    let max_num = max_diff_line_number(&lines);

    // Assert
    assert_eq!(max_num, 0);
}

#[test]
fn test_diff_line_gutter_width_matches_largest_line_number() {
    // Arrange
    let lines = parse_diff_lines("@@ -95,1 +100,1 @@\n context");

    // Act
    let gutter_width = diff_line_gutter_width(&lines);

    // Assert
    assert_eq!(gutter_width, 3);
}

#[test]
fn test_filter_diff_lines_by_file() {
    // Arrange
    let diff = format!(
        "{DIFF_MAIN_HEADER}\n+added in main\n{DIFF_README_HEADER}\n+added in readme\ndiff --git \
         malformed\n+added without path"
    );
    let parsed_lines = parse_diff_lines(&diff);
    let item = FileTreeItem::File("src/main.rs".to_string());

    // Act
    let filtered = filter_diff_lines(&parsed_lines, &item);

    // Assert
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].content, DIFF_MAIN_HEADER);
    assert_eq!(filtered[1].content, "added in main");
}

#[test]
fn test_filter_diff_lines_by_folder() {
    // Arrange
    let diff = format!(
        "{DIFF_MAIN_HEADER}\n+added in main\n{DIFF_NESTED_HEADER}\n-deleted in \
         explorer\n{DIFF_README_HEADER}\n+added in readme"
    );
    let parsed_lines = parse_diff_lines(&diff);
    let item = FileTreeItem::Folder("src/".to_string());

    // Act
    let filtered = filter_diff_lines(&parsed_lines, &item);

    // Assert
    assert_eq!(filtered.len(), 4);
    assert_eq!(filtered[0].content, DIFF_MAIN_HEADER);
    assert_eq!(filtered[1].content, "added in main");
    assert_eq!(filtered[2].content, DIFF_NESTED_HEADER);
    assert_eq!(filtered[3].content, "deleted in explorer");
}

#[test]
fn test_wrap_diff_content_fits() {
    // Arrange
    let content = "short line";

    // Act
    let chunks = wrap_diff_content(content, 80);

    // Assert
    assert_eq!(chunks, vec!["short line"]);
}

#[test]
fn test_wrap_diff_content_wraps() {
    // Arrange
    let content = "abcdefghij";

    // Act
    let chunks = wrap_diff_content(content, 4);

    // Assert
    assert_eq!(chunks, vec!["abcd", "efgh", "ij"]);
}

#[test]
fn test_wrap_diff_content_empty() {
    // Arrange & Act
    let chunks = wrap_diff_content("", 10);

    // Assert
    assert_eq!(chunks, vec![""]);
}

#[test]
fn test_wrap_diff_content_exact() {
    // Arrange
    let content = "abcd";

    // Act
    let chunks = wrap_diff_content(content, 4);

    // Assert
    assert_eq!(chunks, vec!["abcd"]);
}

#[test]
fn test_diff_page_areas_keep_top_margin_and_fill_height_to_page_bottom() {
    // Arrange
    let area = Rect::new(5, 3, 42, 10);

    // Act
    let areas = diff_page_areas(area, 12);

    // Assert
    assert_eq!(areas.file_list_area, Rect::new(6, 4, 12, 9));
    assert_eq!(areas.diff_area, Rect::new(18, 4, 28, 9));
    assert_eq!(areas.diff_area.top(), area.top() + 1);
    assert_eq!(areas.diff_area.bottom(), area.bottom());
}

#[test]
fn test_diff_page_sidebar_scales_and_caps_without_losing_patch_space() {
    // Arrange
    for (width, preferred_width, expected_width) in [
        (0, 12, 0),
        (1, 12, 0),
        (2, 12, 0),
        (30, 60, 8),
        (80, 18, 18),
        (80, 60, 23),
        (120, 20, 20),
        (120, 60, 35),
        (202, 30, 30),
        (300, u16::MAX, 60),
        (u16::MAX, 20, 20),
    ] {
        let area = Rect::new(0, 0, width, 30);

        // Act
        let areas = diff_page_areas(area, preferred_width);

        // Assert
        let content_width = width.saturating_sub(2);
        assert_eq!(areas.file_list_area.width, expected_width);
        assert_eq!(
            areas.file_list_area.width + areas.diff_area.width,
            content_width
        );
        assert!(u32::from(areas.diff_area.width) >= u32::from(content_width) * 7 / 10);
    }
}

#[test]
fn test_diff_view_max_scroll_offset_returns_zero_for_short_diff() {
    // Arrange
    let parsed_lines = parse_diff_lines("+short");
    let terminal_area = Rect::new(0, 0, 120, 30);

    // Act
    let max_scroll_offset = diff_view_max_scroll_offset(&parsed_lines, terminal_area, 18);

    // Assert
    assert_eq!(max_scroll_offset, 0);
}

#[test]
fn test_diff_view_max_scroll_offset_returns_zero_without_viewport_rows() {
    // Arrange
    let diff = "+line\n".repeat(40);
    let parsed_lines = parse_diff_lines(&diff);
    // Three rows hold only the top margin and the panel's top and bottom
    // borders.
    let terminal_area = Rect::new(0, 0, 120, 3);

    // Act
    let max_scroll_offset = diff_view_max_scroll_offset(&parsed_lines, terminal_area, 18);

    // Assert
    assert_eq!(max_scroll_offset, 0);
}

#[test]
fn test_rendered_diff_line_count_separates_files_and_counts_headers() {
    // Arrange
    let diff = format!(
        "{DIFF_MAIN_HEADER}\nindex 1111111..2222222 100644\n@@ -1,1 +1,1 \
         @@\n+abcdefghij\n{DIFF_README_HEADER}\n@@ -1 +1 @@\n ctx"
    );
    let parsed_lines = parse_diff_lines(&diff);
    let layout = DiffRenderLayout {
        content_width: 9,
        gutter_width: 1,
        prefix_width: 5,
        viewport_height: 10,
    };

    // Act
    let rendered_line_count = rendered_diff_line_count(&parsed_lines, layout);
    let empty_line_count = rendered_diff_line_count(&[], layout);

    // Assert
    // Main file: header, index, hunk, and a 10-cell addition wrapped into 3
    // rows of 4 cells. README: separator, header, hunk, and one context row.
    assert_eq!(rendered_line_count, 10);
    assert_eq!(empty_line_count, 1);
}

#[test]
fn test_parse_diff_lines_keeps_counters_for_malformed_hunk_header() {
    // Arrange
    let diff = "@@ -3,1 +7,1 @@\n context\n@@ malformed @@\n+added";

    // Act
    let lines = parse_diff_lines(diff);

    // Assert
    assert_eq!(lines[2].kind, DiffLineKind::HunkHeader);
    assert_eq!(lines[3].kind, DiffLineKind::Addition);
    assert_eq!(lines[3].new_line, Some(8));
}

#[test]
fn test_wrap_diff_content_keeps_content_whole_for_zero_width() {
    // Arrange
    let content = "abcdefghij";

    // Act
    let chunks = wrap_diff_content(content, 0);

    // Assert
    assert_eq!(chunks, vec!["abcdefghij"]);
}

#[test]
fn test_diff_view_max_scroll_offset_counts_wrapped_overflow() {
    // Arrange
    let diff = format!("+{}", "0123456789".repeat(20));
    let parsed_lines = parse_diff_lines(&diff);
    let terminal_area = Rect::new(0, 0, 30, 8);

    // Act
    let max_scroll_offset = diff_view_max_scroll_offset(&parsed_lines, terminal_area, 18);

    // Assert
    assert!(max_scroll_offset > 0);
}
