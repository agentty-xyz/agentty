use std::sync::Arc;

use ratatui::buffer::Cell;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{
    COMMENT_INPUT_MAX_VISIBLE_LINES, DIFF_COMMENT_CACHE_ENTRY_LIMIT, DiffLayoutCache, DiffPage,
    DiffPageInput, comment_lookup_area, diff_changed_line_layout, diff_line_comment_insertions,
    diff_view_max_scroll_offset, preview_path_for_selection, wrapped_comment_input,
};
use crate::domain::session::Session;
use crate::domain::theme::ColorTheme;
use crate::presentation::app_mode::{
    DiffCommentTarget, DiffFocus, DiffLineComment, DiffLineCommentAnchor, DiffLineCommentTarget,
    DiffLineComments, DiffLineSide, DiffPreview, DiffPreviewUnavailableReason, DiffSidebarFocus,
};
use crate::test_support::SessionFixtureBuilder;
use crate::ui::component::file_explorer::FileExplorer;
use crate::ui::component::vertical_scrollbar::{SCROLLBAR_THUMB_SYMBOL, SCROLLBAR_TRACK_SYMBOL};
use crate::ui::diff_util::{parse_diff_lines, selected_diff_lines};
use crate::ui::{Page, diff_util, markdown, style};

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
        can_comment: true,
        diff,
        diff_layout_cache: test_diff_layout_cache(),
        file_explorer_selected_index,
        focus: DiffFocus::Files,
        line_comments: test_line_comments(),
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
        can_comment: true,
        diff,
        diff_layout_cache: test_diff_layout_cache(),
        file_explorer_selected_index,
        focus: DiffFocus::Files,
        line_comments: test_line_comments(),
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

fn test_line_comments() -> &'static DiffLineComments {
    Box::leak(Box::new(DiffLineComments::default()))
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

fn background_cell_count(buffer: &ratatui::buffer::Buffer, color: ratatui::style::Color) -> usize {
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
fn test_diff_content_snapshot_resolves_selected_old_and_new_lines() {
    // Arrange
    let cache = DiffLayoutCache::default();
    let content = cache.content(concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "@@ -4,2 +4,2 @@\n",
        "-old line\n",
        "+new line\n",
    ));

    // Act
    let old_line = content.selected_changed_line(1, 0);
    let new_line = content.selected_changed_line(1, 1);
    let selected_lines = content.selected_changed_lines(1, 0, 1);
    let missing_line = content.selected_changed_line(1, 2);
    let folder_line = content.selected_changed_line(0, 0);
    let folder_lines = content.selected_changed_lines(0, 0, 1);
    let reversed_lines = content.selected_changed_lines(1, 1, 0);
    let folder_anchor_index = content.changed_line_index_for_anchor(
        0,
        old_line
            .as_ref()
            .expect("fixture should contain one deleted line"),
    );
    let missing_anchor_index = content.changed_line_index_for_anchor(
        1,
        &DiffLineCommentAnchor {
            content: "missing line".to_string(),
            line: 4,
            path: "src/main.rs".to_string(),
            side: DiffLineSide::Old,
        },
    );

    // Assert
    assert_eq!(
        old_line,
        Some(DiffLineCommentAnchor {
            content: "old line".to_string(),
            line: 4,
            path: "src/main.rs".to_string(),
            side: DiffLineSide::Old,
        })
    );
    assert_eq!(
        new_line,
        Some(DiffLineCommentAnchor {
            content: "new line".to_string(),
            line: 4,
            path: "src/main.rs".to_string(),
            side: DiffLineSide::New,
        })
    );
    assert_eq!(missing_line, None);
    assert_eq!(folder_line, None);
    assert_eq!(
        selected_lines,
        [
            DiffLineCommentAnchor {
                content: "old line".to_string(),
                line: 4,
                path: "src/main.rs".to_string(),
                side: DiffLineSide::Old,
            },
            DiffLineCommentAnchor {
                content: "new line".to_string(),
                line: 4,
                path: "src/main.rs".to_string(),
                side: DiffLineSide::New,
            },
        ]
    );
    assert_eq!(folder_lines, []);
    assert_eq!(reversed_lines, []);
    assert_eq!(folder_anchor_index, None);
    assert_eq!(missing_anchor_index, None);
}

#[test]
fn test_diff_content_snapshot_keeps_anchor_after_no_newline_marker() {
    // Arrange
    let cache = DiffLayoutCache::default();
    let content = cache.content(concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "@@ -1 +1 @@\n",
        "-old line\n",
        "\\ No newline at end of file\n",
        "+new line\n",
    ));

    // Act
    let new_line = content.selected_changed_line(1, 1);

    // Assert
    assert_eq!(
        new_line,
        Some(DiffLineCommentAnchor {
            content: "new line".to_string(),
            line: 1,
            path: "src/main.rs".to_string(),
            side: DiffLineSide::New,
        })
    );
}

#[test]
fn test_diff_content_snapshot_uses_side_specific_rename_paths() {
    // Arrange
    let cache = DiffLayoutCache::default();
    let content = cache.content(concat!(
        "diff --git a/old.rs b/new.rs\n",
        "similarity index 50%\n",
        "rename from old.rs\n",
        "rename to new.rs\n",
        "@@ -3 +3 @@\n",
        "-old line\n",
        "+new line\n",
    ));

    // Act
    let old_line = content
        .selected_changed_line(0, 0)
        .expect("renamed deletion should be selectable");
    let new_line = content
        .selected_changed_line(0, 1)
        .expect("renamed addition should be selectable");
    let old_line_index = content.changed_line_index_for_anchor(0, &old_line);
    let new_line_index = content.changed_line_index_for_anchor(0, &new_line);

    // Assert
    assert_eq!(old_line.path, "old.rs");
    assert_eq!(old_line.side, DiffLineSide::Old);
    assert_eq!(new_line.path, "new.rs");
    assert_eq!(new_line.side, DiffLineSide::New);
    assert_eq!(old_line_index, Some(0));
    assert_eq!(new_line_index, Some(1));
}

#[test]
fn test_comment_lookup_tracks_input_and_scroll_position() {
    // Arrange
    let viewport = Rect::new(20, 2, 70, 20);

    // Act, Assert
    for (rows, scroll, expected) in [
        (8..11, 0, Some(Rect::new(21, 5, 69, 5))),
        (8..11, 4, Some(Rect::new(21, 2, 69, 4))),
        (0..3, 0, Some(Rect::new(21, 5, 69, 5))),
        (0..3, 1, Some(Rect::new(21, 4, 69, 5))),
        (0..3, 3, None),
        (20..23, 0, None),
        (0..19, 0, None),
    ] {
        assert_eq!(comment_lookup_area(viewport, rows, scroll, 5), expected);
    }
}

#[test]
fn test_diff_comment_lookup_renders_matches_and_constrained_footer() {
    for (height, has_match, editing) in [
        (14, true, true),
        (3, true, true),
        (14, false, true),
        (14, true, false),
    ] {
        // Arrange
        let diff = concat!(
            "diff --git a/src/main.rs b/src/main.rs\n",
            "@@ -0,0 +1,2 @@\n",
            "+fn main() {}\n",
            "+review();\n",
        );
        let mut line_comments = DiffLineComments::default();
        line_comments.start_editing_target(DiffLineCommentTarget::single(DiffLineCommentAnchor {
            content: "fn main() {}".to_string(),
            line: 1,
            path: "src/main.rs".to_string(),
            side: DiffLineSide::New,
        }));
        line_comments
            .editing_input_mut()
            .expect("inline comment should be editable")
            .insert_text("@src");
        line_comments.at_mention_state = Some(Box::new(
            crate::presentation::prompt::PromptAtMentionState::new(if has_match {
                vec![crate::domain::file_entry::FileEntry {
                    is_dir: false,
                    path: "src/lookup.rs".into(),
                }]
            } else {
                Vec::new()
            }),
        ));
        if !editing {
            line_comments.editing_index = None;
        }
        let session = session_fixture();
        let diff_layout_cache = DiffLayoutCache::default();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let mut page = DiffPage::new(DiffPageInput {
            can_comment: true,
            diff,
            diff_layout_cache: &diff_layout_cache,
            file_explorer_selected_index: 1,
            focus: DiffFocus::Content,
            line_comments: &line_comments,
            markdown_render_cache: &markdown_render_cache,
            preview: test_diff_preview(),
            review_comments: None,
            scroll_offset: 0,
            selected_diff_line_index: 0,
            session: &session,
            sidebar_focus: DiffSidebarFocus::Files,
        });
        let backend = ratatui::backend::TestBackend::new(100, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| page.render(frame, frame.area()))
            .expect("failed to render diff page");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert_eq!(text.contains("Esc: dismiss lookup"), editing);
        assert_eq!(
            text.contains("Tab/Enter: select"),
            height == 14 && has_match && editing
        );
        assert_eq!(
            text.contains("src/lookup.rs"),
            height == 14 && has_match && editing
        );
        if height == 14 && has_match && editing {
            let areas = diff_util::diff_page_areas(Rect::new(0, 0, 100, height));
            assert_comment_lookup_position(terminal.backend().buffer(), areas.diff_area);
        }
    }
}

/// Checks that suggestions stay in the diff pane without covering the
/// draft.
fn assert_comment_lookup_position(buffer: &ratatui::buffer::Buffer, diff_area: Rect) {
    let row_texts: Vec<String> = buffer
        .content
        .chunks(100)
        .map(|row| row.iter().map(Cell::symbol).collect())
        .collect();
    let (lookup_row, lookup_text) = row_texts
        .iter()
        .enumerate()
        .find(|(_, row)| row.contains("src/lookup.rs"))
        .expect("lookup row");
    let input_row = row_texts
        .iter()
        .position(|row| row.contains("@src|"))
        .expect("input row");
    assert_ne!(lookup_row, input_row);
    assert!(lookup_text.find("src/lookup.rs").expect("lookup column") >= usize::from(diff_area.x));
}

#[test]
fn test_diff_page_renders_active_comment_below_its_changed_line() {
    // Arrange
    let diff = concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "@@ -0,0 +1,2 @@\n",
        "+fn main() {}\n",
        "+review();\n",
    );
    let mut line_comments = DiffLineComments::default();
    line_comments.start_editing_target(DiffLineCommentTarget::single(DiffLineCommentAnchor {
        content: "fn main() {}".to_string(),
        line: 1,
        path: "src/main.rs".to_string(),
        side: DiffLineSide::New,
    }));
    line_comments
        .editing_input_mut()
        .expect("inline comment should be editable")
        .insert_text("Explain this entry point");
    let session = session_fixture();
    let diff_layout_cache = DiffLayoutCache::default();
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let mut page = DiffPage::new(DiffPageInput {
        can_comment: true,
        diff,
        diff_layout_cache: &diff_layout_cache,
        file_explorer_selected_index: 1,
        focus: DiffFocus::Content,
        line_comments: &line_comments,
        markdown_render_cache: &markdown_render_cache,
        preview: test_diff_preview(),
        review_comments: None,
        scroll_offset: 0,
        selected_diff_line_index: 0,
        session: &session,
        sidebar_focus: DiffSidebarFocus::Files,
    });
    let backend = ratatui::backend::TestBackend::new(100, 14);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| page.render(frame, frame.area()))
        .expect("failed to render diff page");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("New line 1"));
    assert!(text.contains("Explain this entry point|"));
    assert!(text.find("fn main() {}").is_some_and(|source_index| {
        text.find("New line 1")
            .is_some_and(|comment| comment > source_index)
    }));

    // Act — read-only sessions suppress comment editing affordances.
    page.can_comment = false;
    terminal
        .draw(|frame| page.render(frame, frame.area()))
        .expect("failed to render read-only diff page");

    // Assert
    let read_only_text = buffer_text(terminal.backend().buffer());
    assert!(!read_only_text.contains("save comment"));
}

#[test]
fn test_diff_page_renders_file_comment_above_selected_file_patch() {
    // Arrange
    let diff = concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "@@ -0,0 +1 @@\n",
        "+fn main() {}\n",
    );
    let mut comments = DiffLineComments::default();
    comments.start_editing_target(DiffCommentTarget::file("src/main.rs"));
    comments
        .editing_input_mut()
        .expect("file comment should be editable")
        .insert_text("Review the complete file");
    let session = session_fixture();
    let diff_layout_cache = DiffLayoutCache::default();
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let mut page = DiffPage::new(DiffPageInput {
        can_comment: true,
        diff,
        diff_layout_cache: &diff_layout_cache,
        file_explorer_selected_index: 1,
        focus: DiffFocus::Content,
        line_comments: &comments,
        markdown_render_cache: &markdown_render_cache,
        preview: test_diff_preview(),
        review_comments: None,
        scroll_offset: 0,
        selected_diff_line_index: 0,
        session: &session,
        sidebar_focus: DiffSidebarFocus::Files,
    });
    let backend = ratatui::backend::TestBackend::new(100, 14);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| page.render(frame, frame.area()))
        .expect("failed to render diff page");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    let comment_index = text
        .find("File comment")
        .expect("file editor should be visible");
    assert!(text.contains("Review the complete file|"));
    let source_index = text
        .find("fn main() {}")
        .expect("selected file patch should be visible");
    assert!(comment_index < source_index);
}

#[test]
fn test_diff_page_distinguishes_cursor_inside_commented_range() {
    // Arrange
    let diff = concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "@@ -0,0 +1,2 @@\n",
        "+fn main() {}\n",
        "+review();\n",
    );
    let mut line_comments = DiffLineComments::default();
    line_comments.start_selection(0);
    line_comments.start_editing_target(
        DiffLineCommentTarget::from_anchors(vec![
            DiffLineCommentAnchor {
                content: "fn main() {}".to_string(),
                line: 1,
                path: "src/main.rs".to_string(),
                side: DiffLineSide::New,
            },
            DiffLineCommentAnchor {
                content: "review();".to_string(),
                line: 2,
                path: "src/main.rs".to_string(),
                side: DiffLineSide::New,
            },
        ])
        .expect("two changed rows should form a comment target"),
    );
    line_comments
        .editing_input_mut()
        .expect("range comment should be editable")
        .insert_text("Explain both lines");
    line_comments.finish_editing();
    line_comments.clear_comment_selection();
    let session = session_fixture();
    let diff_layout_cache = DiffLayoutCache::default();
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let mut page = DiffPage::new(DiffPageInput {
        can_comment: true,
        diff,
        diff_layout_cache: &diff_layout_cache,
        file_explorer_selected_index: 1,
        focus: DiffFocus::Content,
        line_comments: &line_comments,
        markdown_render_cache: &markdown_render_cache,
        preview: test_diff_preview(),
        review_comments: None,
        scroll_offset: 0,
        selected_diff_line_index: 1,
        session: &session,
        sidebar_focus: DiffSidebarFocus::Files,
    });
    let backend = ratatui::backend::TestBackend::new(100, 14);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| page.render(frame, frame.area()))
        .expect("failed to render diff page");

    // Assert
    let commented_source_rows = terminal
        .backend()
        .buffer()
        .content()
        .chunks(100)
        .filter(|row| {
            let row_text = row
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>();
            row_text.contains("fn main() {}") || row_text.contains("review();")
        })
        .collect::<Vec<_>>();
    assert_eq!(commented_source_rows.len(), 2);
    assert!(commented_source_rows.iter().all(|row| {
        row.iter()
            .any(|cell| cell.bg == style::palette::surface_prompt())
    }));
    assert!(
        commented_source_rows[0]
            .iter()
            .all(|cell| !cell.modifier.contains(Modifier::REVERSED))
    );
    assert!(
        commented_source_rows[1]
            .iter()
            .any(|cell| cell.modifier.contains(Modifier::REVERSED))
    );
}

#[test]
fn test_diff_page_shows_visual_selection_footer() {
    // Arrange
    let diff = "diff --git a/main.rs b/main.rs\n@@ -0,0 +1 @@\n+review();";
    let mut line_comments = DiffLineComments::default();
    line_comments.start_selection(0);
    let session = session_fixture();
    let diff_layout_cache = DiffLayoutCache::default();
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let mut page = DiffPage::new(DiffPageInput {
        can_comment: true,
        diff,
        diff_layout_cache: &diff_layout_cache,
        file_explorer_selected_index: 0,
        focus: DiffFocus::Content,
        line_comments: &line_comments,
        markdown_render_cache: &markdown_render_cache,
        preview: test_diff_preview(),
        review_comments: None,
        scroll_offset: 0,
        selected_diff_line_index: 0,
        session: &session,
        sidebar_focus: DiffSidebarFocus::Files,
    });
    let backend = ratatui::backend::TestBackend::new(100, 14);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| page.render(frame, frame.area()))
        .expect("failed to render diff page");

    // Assert
    assert!(buffer_text(terminal.backend().buffer()).contains("Esc: cancel"));
}

#[test]
fn test_diff_page_shows_comment_submission_in_file_window() {
    // Arrange
    let diff = "diff --git a/main.rs b/main.rs\n@@ -0,0 +1 @@\n+review();";
    let mut line_comments = DiffLineComments::default();
    line_comments.start_editing_target(DiffLineCommentTarget::single(DiffLineCommentAnchor {
        content: "review();".to_string(),
        line: 1,
        path: "main.rs".to_string(),
        side: DiffLineSide::New,
    }));
    line_comments
        .editing_input_mut()
        .expect("line comment should be editable")
        .insert_text("Explain this call");
    line_comments.finish_editing();
    let session = session_fixture();
    let diff_layout_cache = DiffLayoutCache::default();
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let mut page = DiffPage::new(DiffPageInput {
        can_comment: true,
        diff,
        diff_layout_cache: &diff_layout_cache,
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: &line_comments,
        markdown_render_cache: &markdown_render_cache,
        preview: test_diff_preview(),
        review_comments: None,
        scroll_offset: 0,
        selected_diff_line_index: 0,
        session: &session,
        sidebar_focus: DiffSidebarFocus::Files,
    });
    let backend = ratatui::backend::TestBackend::new(100, 14);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| page.render(frame, frame.area()))
        .expect("failed to render diff page");

    // Assert
    assert!(
        buffer_text(terminal.backend().buffer()).contains("s: submit comments"),
        "completed comments should be submittable while Files owns focus"
    );
}

#[test]
fn test_inline_comment_editor_shows_added_range_title_and_state_highlighting() {
    // Arrange
    let comment = DiffLineComment {
        input: crate::domain::input::InputState::with_text("Explain this".to_string()),
        target: DiffLineCommentTarget::from_anchors(vec![
            DiffLineCommentAnchor {
                content: "review();".to_string(),
                line: 4,
                path: "src/main.rs".to_string(),
                side: DiffLineSide::New,
            },
            DiffLineCommentAnchor {
                content: "finish();".to_string(),
                line: 8,
                path: "src/main.rs".to_string(),
                side: DiffLineSide::New,
            },
        ])
        .expect("two anchors should create a range target")
        .into(),
    };

    // Act
    let completed_lines = DiffPage::inline_comment_lines(&comment, false, false, 40);
    let selected_lines = DiffPage::inline_comment_lines(&comment, false, true, 40);
    let active_lines = DiffPage::inline_comment_lines(&comment, true, true, 40);

    // Assert
    assert_eq!(completed_lines.len(), 3);
    assert!(completed_lines[0].to_string().contains("New lines 4-8"));
    assert!(completed_lines[0].to_string().starts_with(" ╭─ New"));
    assert!(completed_lines.iter().all(|line| line.width() == 40));
    assert!(selected_lines.iter().all(|line| line.width() == 40));
    assert!(active_lines.iter().all(|line| line.width() == 40));
    assert!(
        completed_lines
            .iter()
            .flat_map(|line| &line.spans)
            .all(|span| span.style.bg == Some(style::palette::surface_prompt()))
    );
    assert!(
        selected_lines
            .iter()
            .flat_map(|line| &line.spans)
            .all(|span| span.style.add_modifier.contains(Modifier::REVERSED))
    );
    assert!(
        active_lines
            .iter()
            .flat_map(|line| &line.spans)
            .all(|span| {
                span.style.bg == Some(style::palette::surface_selection())
                    && !span.style.add_modifier.contains(Modifier::REVERSED)
            })
    );
}

#[test]
fn test_completed_inline_comment_renders_every_body_row() {
    // Arrange
    let comment = DiffLineComment {
        input: crate::domain::input::InputState::with_text(
            "one\ntwo\nthree\nfour\nfive\nsix".to_string(),
        ),
        target: DiffCommentTarget::file("src/main.rs"),
    };

    // Act
    let completed_lines = DiffPage::inline_comment_lines(&comment, false, false, 40);
    let active_lines = DiffPage::inline_comment_lines(&comment, true, true, 40);

    // Assert
    assert_eq!(completed_lines.len(), 8);
    assert!(
        completed_lines
            .iter()
            .any(|line| line.to_string().contains("one"))
    );
    assert!(
        completed_lines
            .iter()
            .any(|line| line.to_string().contains("six"))
    );
    assert_eq!(active_lines.len(), COMMENT_INPUT_MAX_VISIBLE_LINES + 2);
    assert!(
        active_lines
            .iter()
            .all(|line| !line.to_string().contains("one"))
    );
    assert!(
        active_lines
            .iter()
            .any(|line| line.to_string().contains("six|"))
    );
}

#[test]
fn test_inline_comment_title_distinguishes_deleted_and_mixed_ranges() {
    // Arrange
    let deleted_target = DiffLineCommentTarget::single(DiffLineCommentAnchor {
        content: "old();".to_string(),
        line: 4,
        path: "src/main.rs".to_string(),
        side: DiffLineSide::Old,
    });
    let mixed_target = DiffLineCommentTarget::from_anchors(vec![
        DiffLineCommentAnchor {
            content: "old_first();".to_string(),
            line: 4,
            path: "src/main.rs".to_string(),
            side: DiffLineSide::Old,
        },
        DiffLineCommentAnchor {
            content: "old_last();".to_string(),
            line: 5,
            path: "src/main.rs".to_string(),
            side: DiffLineSide::Old,
        },
        DiffLineCommentAnchor {
            content: "new_first();".to_string(),
            line: 4,
            path: "src/main.rs".to_string(),
            side: DiffLineSide::New,
        },
        DiffLineCommentAnchor {
            content: "new_last();".to_string(),
            line: 6,
            path: "src/main.rs".to_string(),
            side: DiffLineSide::New,
        },
    ])
    .expect("mixed anchors should create a target");

    // Act
    let deleted_title = DiffPage::inline_comment_title(&deleted_target.into());
    let mixed_title = DiffPage::inline_comment_title(&mixed_target.into());

    // Assert
    assert_eq!(deleted_title, "Old line 4");
    assert_eq!(mixed_title, "Old lines 4-5 · New lines 4-6");
}

#[test]
fn test_wrapped_comment_input_preserves_newlines_and_clamps_cursor() {
    // Arrange
    let mut input = crate::domain::input::InputState::with_text("first line\nsecond".to_string());
    input.cursor = usize::MAX;
    let narrow_input = crate::domain::input::InputState::with_text("abcdef".to_string());

    // Act
    let (text_at_end, cursor_row_at_end) = wrapped_comment_input(&input, true, 20);
    input.cursor = 3;
    let (text_in_middle, cursor_row_in_middle) = wrapped_comment_input(&input, true, 20);
    let (wrapped_text, _) = wrapped_comment_input(&narrow_input, false, 3);

    // Assert
    assert_eq!(text_at_end, ["first line", "second|"]);
    assert_eq!(cursor_row_at_end, 1);
    assert_eq!(text_in_middle, ["fir|st line", "second"]);
    assert_eq!(cursor_row_in_middle, 0);
    assert_eq!(wrapped_text, ["abc", "def"]);
}

#[test]
fn test_padded_comment_line_fills_remaining_content_width() {
    // Arrange
    let content_style = Style::default().fg(style::palette::text());

    // Act
    let line = DiffPage::padded_comment_line(
        vec![Span::styled("body", content_style)],
        2,
        10,
        content_style,
    );

    // Assert
    assert_eq!(line.width(), 10);
    assert_eq!(line.to_string(), "  body    ");
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
    assert_eq!(missing_path_lines, []);
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
    let line_comments = DiffLineComments::default();

    // Act
    let layout = diff_changed_line_layout(&diff, &line_comments, 0, terminal_area, &cache);
    let scrolled_down = layout
        .changed_line_scroll_offset(20, 0)
        .expect("selected changed line should have a rendered range");
    let first_visible_changed_line = layout
        .changed_line_index_at_visual_row(scrolled_down, 0)
        .expect("scrolled viewport should contain a changed line");
    let aligned_changed_line = layout
        .changed_line_index_at_visual_row(scrolled_down, 3)
        .expect("aligned viewport row should contain a changed line");
    let scrolled_up = layout
        .changed_line_scroll_offset(0, scrolled_down)
        .expect("first changed line should have a rendered range");
    let zero_height_layout =
        diff_changed_line_layout(&diff, &line_comments, 0, zero_height_area, &cache);
    let zero_height_scroll_offset = zero_height_layout
        .changed_line_scroll_offset(0, scrolled_down)
        .expect("first changed line should remain selectable without a viewport");
    let selection_range = layout
        .changed_line_selection_range(2, 4)
        .expect("valid changed-line selection should resolve rendered rows");
    let missing_selection_range = layout.changed_line_selection_range(2, usize::MAX);

    // Assert
    assert_eq!(layout.changed_line_count(), 40);
    assert!(scrolled_down > 0);
    assert!(first_visible_changed_line > 0);
    assert!(first_visible_changed_line <= 20);
    assert!(aligned_changed_line > first_visible_changed_line);
    assert!(scrolled_up < scrolled_down);
    assert_eq!(zero_height_scroll_offset, 0);
    assert_eq!(
        selection_range,
        layout.changed_line_ranges[2].start..layout.changed_line_ranges[4].end
    );
    assert_eq!(missing_selection_range, None);
}

#[test]
fn test_diff_changed_line_layout_selects_last_change_below_viewport() {
    // Arrange
    let diff = "diff --git a/main.rs b/main.rs\n@@ -0,0 +1 @@\n+changed\n context";
    let cache = DiffLayoutCache::default();
    let layout = diff_changed_line_layout(
        diff,
        &DiffLineComments::default(),
        0,
        Rect::new(0, 0, 80, 12),
        &cache,
    );

    // Act
    let selected_index = layout.changed_line_index_at_visual_row(u16::MAX, 0);

    // Assert
    assert_eq!(selected_index, Some(0));
}

#[test]
fn test_diff_line_comment_insertions_ignore_missing_target_bounds() {
    // Arrange
    let diff = "diff --git a/main.rs b/main.rs\n@@ -0,0 +1 @@\n+review();";
    let cache = DiffLayoutCache::default();
    let content = cache.content(diff);
    let changed_line_ranges = diff_changed_line_layout(
        diff,
        &DiffLineComments::default(),
        0,
        Rect::new(0, 0, 80, 12),
        &cache,
    )
    .changed_line_ranges;
    let valid_anchor = content
        .selected_changed_line(0, 0)
        .expect("fixture should contain one changed line");
    let missing_anchor = DiffLineCommentAnchor {
        content: "missing();".to_string(),
        line: 999,
        path: "main.rs".to_string(),
        side: DiffLineSide::New,
    };
    let targets = [
        DiffLineCommentTarget::from_anchors(vec![missing_anchor.clone(), valid_anchor.clone()])
            .expect("first missing target should be nonempty"),
        DiffLineCommentTarget::from_anchors(vec![valid_anchor, missing_anchor])
            .expect("last missing target should be nonempty"),
    ];
    let mut line_comments = DiffLineComments::default();
    for target in targets {
        line_comments.start_editing_target(target);
        line_comments
            .editing_input_mut()
            .expect("stale comment target should remain editable")
            .insert_text("Explain this");
        line_comments.finish_editing();
    }
    line_comments.start_editing_target(DiffCommentTarget::file("other.rs"));
    line_comments
        .editing_input_mut()
        .expect("stale file comment should remain editable")
        .insert_text("Review the other file");
    line_comments.finish_editing();

    // Act
    let insertions = diff_line_comment_insertions(
        &content,
        0,
        &changed_line_ranges,
        &line_comments,
        80,
        &cache,
    );

    // Assert
    assert_eq!(line_comments.comments.len(), 3);
    assert!(insertions.is_empty());
}

#[test]
fn test_content_selection_moves_from_range_comment_to_following_source_row() {
    // Arrange
    let diff = concat!(
        "diff --git a/main.rs b/main.rs\n",
        "@@ -0,0 +1,3 @@\n",
        "+first();\n",
        "+second();\n",
        "+third();",
    );
    let cache = DiffLayoutCache::default();
    let content = cache.content(diff);
    let anchors = content.selected_changed_lines(0, 0, 1);
    let mut line_comments = DiffLineComments::default();
    line_comments.start_editing_target(
        DiffLineCommentTarget::from_anchors(anchors)
            .expect("fixture should create a range comment"),
    );
    line_comments
        .editing_input_mut()
        .expect("range comment should be editable")
        .insert_text("Explain this range");
    line_comments.finish_editing();
    let layout = diff_changed_line_layout(diff, &line_comments, 0, Rect::new(0, 0, 80, 12), &cache);

    // Act
    let next_selection = layout.next_content_selection(0, line_comments.selected_comment_index());
    let previous_selection = layout.previous_content_selection(2, None);
    let stale_comment_selection = layout.next_content_selection(1, Some(usize::MAX));

    // Assert
    assert_eq!(next_selection, (2, None));
    assert_eq!(previous_selection, (1, Some(0)));
    assert_eq!(stale_comment_selection, (1, Some(0)));
}

#[test]
fn test_inline_comment_overflow_reserves_scrollbar_layout_everywhere() {
    // Arrange
    let cache = DiffLayoutCache::default();
    let markdown_cache = markdown::MarkdownRenderCache::default();
    let empty_comments = DiffLineComments::default();
    let tall_area = Rect::new(0, 0, 50, 100);
    let sizing_diff = "diff --git a/main.rs b/main.rs\n@@ -0,0 +1 @@\n+x";
    let sizing_layout =
        diff_changed_line_layout(sizing_diff, &empty_comments, 0, tall_area, &cache);
    let line_width = sizing_layout
        .render_layout
        .content_width
        .saturating_sub(sizing_layout.render_layout.prefix_width);
    let diff = format!(
        "diff --git a/main.rs b/main.rs\n@@ -0,0 +1 @@\n+{}",
        "x".repeat(line_width),
    );
    let tall_layout = diff_changed_line_layout(&diff, &empty_comments, 0, tall_area, &cache);
    let fitting_height = u16::try_from(tall_layout.line_count.saturating_add(5))
        .expect("short diff height should fit in a terminal");
    let fitting_area = Rect::new(0, 0, 50, fitting_height);
    let fitting_layout = diff_changed_line_layout(&diff, &empty_comments, 0, fitting_area, &cache);
    let content = cache.content(&diff);
    let anchor = content
        .selected_changed_line(0, 0)
        .expect("fixture should contain one changed line");
    let mut line_comments = DiffLineComments::default();
    line_comments.start_editing_target(DiffLineCommentTarget::single(anchor));
    line_comments
        .editing_input_mut()
        .expect("inline comment should be editable")
        .insert_text("comment");
    line_comments.finish_editing();

    // Act
    let commented_layout = diff_changed_line_layout(&diff, &line_comments, 0, fitting_area, &cache);
    let max_scroll_offset = diff_view_max_scroll_offset(
        &diff,
        &line_comments,
        0,
        fitting_area,
        &cache,
        &markdown_cache,
        &DiffPreview::default(),
    );

    // Assert
    assert!(!fitting_layout.show_scrollbar);
    assert!(commented_layout.show_scrollbar);
    assert_eq!(
        commented_layout
            .render_layout
            .content_width
            .saturating_add(1),
        fitting_layout.render_layout.content_width,
    );
    assert_eq!(commented_layout.comment_insertions.len(), 1);
    assert_eq!(
        commented_layout.changed_line_ranges[0].len(),
        fitting_layout.changed_line_ranges[0]
            .len()
            .saturating_add(1),
    );
    assert_eq!(
        commented_layout.comment_insertions[0].display_row,
        commented_layout.changed_line_ranges[0].end,
    );
    assert_eq!(
        max_scroll_offset,
        diff_util::clamp_diff_scroll_offset(
            u16::MAX,
            commented_layout.line_count,
            commented_layout.render_layout.viewport_height,
        ),
    );
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
    let line_comments = DiffLineComments::default();
    let first_layout = cache.resolved_layout(&content, &line_comments, 0, area);
    let second_layout = cache.resolved_layout(&content, &line_comments, 0, area);

    // Assert
    assert!(Arc::ptr_eq(&first_layout.lines, &second_layout.lines));
    assert_eq!(first_layout.line_count, second_layout.line_count);
}

#[test]
fn test_diff_comment_cache_tracks_input_snapshot_and_width() {
    // Arrange
    let cache = DiffLayoutCache::default();
    let mut comment = DiffLineComment {
        input: crate::domain::input::InputState::with_text("first".to_string()),
        target: DiffCommentTarget::file("src/main.rs"),
    };

    // Act
    let first_lines = cache.comment_lines(0, &comment, false, false, 40);
    let repeated_lines = cache.comment_lines(0, &comment, false, false, 40);
    let replacement = DiffLineComment {
        input: crate::domain::input::InputState::with_text("other".to_string()),
        target: DiffCommentTarget::file("src/main.rs"),
    };
    let replacement_lines = cache.comment_lines(0, &replacement, false, false, 40);
    comment.input.insert_text(" second");
    let revised_lines = cache.comment_lines(0, &comment, false, false, 40);
    let narrower_lines = cache.comment_lines(0, &comment, false, false, 20);

    // Assert
    assert!(Arc::ptr_eq(&first_lines, &repeated_lines));
    assert!(!Arc::ptr_eq(&first_lines, &replacement_lines));
    assert!(!Arc::ptr_eq(&first_lines, &revised_lines));
    assert!(!Arc::ptr_eq(&revised_lines, &narrower_lines));
    assert!(replacement_lines[1].to_string().contains("other"));
    assert_eq!(cache.comment_rows.borrow().len(), 4);

    // Act
    for content_width in 1..=DIFF_COMMENT_CACHE_ENTRY_LIMIT + 1 {
        cache.comment_lines(0, &comment, false, false, content_width);
    }

    // Assert
    assert_eq!(
        cache.comment_rows.borrow().len(),
        DIFF_COMMENT_CACHE_ENTRY_LIMIT
    );
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
    assert!(text.contains("Enter/l: open"));
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
                reason: DiffPreviewUnavailableReason::LoadFailed("Preview read failed".to_string()),
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
            let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
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
    let line_comments = DiffLineComments::default();
    let mut diff_page = DiffPage::new(DiffPageInput {
        can_comment: true,
        diff,
        diff_layout_cache: &diff_layout_cache,
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: &line_comments,
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
        &line_comments,
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
    let line_comments = DiffLineComments::default();

    // Act
    let max_scroll_offset = diff_view_max_scroll_offset(
        diff,
        &line_comments,
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
