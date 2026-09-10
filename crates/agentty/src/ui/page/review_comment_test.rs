use ag_forge::{
    ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewCommentThread,
};
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::text::Line;

use super::{
    ReviewCommentPage, ReviewCommentPageInput, ReviewCommentRenderCaches, code_context_line,
    code_context_lines, comment_detail_lines, comment_list_items, diff_line_matches_anchor,
    review_comment_item_count, review_comment_selected_is_actionable,
    review_comment_view_max_scroll_offset,
};
use crate::domain::theme::ColorTheme;
use crate::presentation::app_mode::ReviewCommentSelection;
use crate::presentation::review_comment as review_comment_selection;
use crate::test_support::SessionFixtureBuilder;
use crate::ui::component::vertical_scrollbar::SCROLLBAR_THUMB_SYMBOL;
use crate::ui::diff_util::{DiffLine, DiffLineKind};
use crate::ui::{diff_util, markdown, style};

const SAMPLE_DIFF: &str = concat!(
    "diff --git a/src/main.rs b/src/main.rs\n",
    "index 1111111..2222222 100644\n",
    "--- a/src/main.rs\n",
    "+++ b/src/main.rs\n",
    "@@ -1,2 +1,3 @@\n",
    " fn main() {\n",
    "+    println!(\"review\");\n",
    " }\n",
);

fn inline_thread(line: u32) -> ReviewCommentThread {
    ReviewCommentThread {
        anchor_side: ReviewCommentAnchorSide::New,
        comments: vec![ReviewComment {
            author: "alice".to_string(),
            authored_by_current_user: false,
            body: "Please explain this output.".to_string(),
        }],
        id: "thread-id".to_string(),
        is_outdated: Some(false),
        is_resolved: false,
        line: Some(line),
        path: "src/main.rs".to_string(),
        start_line: None,
    }
}

fn file_thread() -> ReviewCommentThread {
    ReviewCommentThread {
        anchor_side: ReviewCommentAnchorSide::File,
        comments: vec![ReviewComment {
            author: "bob".to_string(),
            authored_by_current_user: false,
            body: "Please review the whole file.".to_string(),
        }],
        id: "thread-id".to_string(),
        is_outdated: Some(false),
        is_resolved: false,
        line: None,
        path: "src/main.rs".to_string(),
        start_line: None,
    }
}

fn comment_snapshot() -> ReviewCommentSnapshot {
    let mut thread = inline_thread(2);
    thread.comments[0].body = (0..12)
        .map(|line_number| format!("Inline comment line {line_number}"))
        .collect::<Vec<_>>()
        .join("\n");

    ReviewCommentSnapshot {
        pr_level_comments: vec![ReviewComment {
            author: "alice".to_string(),
            authored_by_current_user: false,
            body: "General comment".to_string(),
        }],
        threads: vec![thread],
    }
}

fn render_review_comment_page(
    snapshot: Option<&ReviewCommentSnapshot>,
    comment_error: Option<&str>,
    is_loading_comments: bool,
    selected_comment_index: usize,
    scroll_offset: u16,
    terminal_size: (u16, u16),
) -> ratatui::buffer::Buffer {
    render_review_comment_page_with_selections(
        snapshot,
        &[],
        comment_error,
        is_loading_comments,
        selected_comment_index,
        scroll_offset,
        terminal_size,
    )
}

fn render_review_comment_page_with_selections(
    snapshot: Option<&ReviewCommentSnapshot>,
    selected_comments: &[ReviewCommentSelection],
    comment_error: Option<&str>,
    is_loading_comments: bool,
    selected_comment_index: usize,
    scroll_offset: u16,
    terminal_size: (u16, u16),
) -> ratatui::buffer::Buffer {
    let session = SessionFixtureBuilder::new()
        .title(Some("Review comments session".to_string()))
        .build();
    let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let backend = TestBackend::new(terminal_size.0, terminal_size.1);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    terminal
        .draw(|frame| {
            let page = ReviewCommentPage::new(ReviewCommentPageInput {
                selected_comments,
                comment_error,
                comment_snapshot: snapshot,
                diff: SAMPLE_DIFF,
                is_loading_comments,
                render_caches: ReviewCommentRenderCaches {
                    diff_layout: &diff_layout_cache,
                    markdown: &markdown_render_cache,
                },
                scroll_offset,
                selected_comment_index,
                session: &session,
            });
            let areas = diff_util::diff_page_areas(frame.area());
            let rows = snapshot
                .map(review_comment_selection::grouped_review_comment_rows)
                .unwrap_or_default();
            page.render_comment_list(frame, areas.file_list_area, &rows, true);
            page.render_comment_detail(frame, areas.diff_area, &rows);
        })
        .expect("failed to draw review-comment page");

    terminal.backend().buffer().clone()
}

fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

#[test]
fn test_render_shows_comment_selector_and_general_detail() {
    // Arrange
    let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
    let snapshot = comment_snapshot();

    // Act
    let buffer = render_review_comment_page(Some(&snapshot), None, false, 1, 0, (140, 24));
    let text = buffer_text(&buffer);

    // Assert
    assert!(text.contains("Comments (2)"));
    assert!(text.contains("Unresolved"));
    assert!(text.contains("Standalone"));
    assert!(text.contains("General · alice"));
    assert!(text.contains("src/main.rs:2"));
    assert!(text.contains("Comment — Review comments session"));
    assert!(text.contains("Scope: General discussion"));
    assert!(text.contains("General comment"));
    assert!(text.contains("This comment is not attached to a code line."));
    assert!(
        buffer
            .content()
            .iter()
            .any(|cell| cell.bg == style::palette::surface_selection())
    );
}

#[test]
fn test_render_shows_selected_inline_context_and_scrollbar() {
    // Arrange
    let snapshot = comment_snapshot();

    // Act
    let buffer = render_review_comment_page(Some(&snapshot), None, false, 0, 0, (100, 14));
    let text = buffer_text(&buffer);

    // Assert
    assert!(text.contains("src/main.rs:2"));
    assert!(text.contains("Code context"));
    assert!(text.contains("println!(\"review\")"));
    assert!(text.contains(SCROLLBAR_THUMB_SYMBOL));
}

#[test]
fn test_render_shows_selected_comment_markers() {
    // Arrange
    let mut selected_thread = inline_thread(2);
    selected_thread.id = "selected".to_string();
    let mut unselected_thread = inline_thread(3);
    unselected_thread.id = "unselected".to_string();
    let mut resolved_thread = inline_thread(4);
    resolved_thread.id = "resolved".to_string();
    resolved_thread.is_resolved = true;
    let snapshot = ReviewCommentSnapshot {
        pr_level_comments: Vec::new(),
        threads: vec![selected_thread, unselected_thread, resolved_thread],
    };
    let selected_comments = vec![ReviewCommentSelection {
        thread_id: "selected".to_string(),
    }];

    // Act
    let buffer = render_review_comment_page_with_selections(
        Some(&snapshot),
        &selected_comments,
        None,
        false,
        0,
        0,
        (140, 24),
    );
    let text = buffer_text(&buffer);

    // Assert
    assert!(text.contains("[x] src/main.rs:2"));
    assert!(text.contains("[ ] src/main.rs:3"));
    assert!(text.contains("src/main.rs:4"));
}

#[test]
fn test_render_shows_loading_error_and_empty_fallbacks() {
    // Arrange
    let empty_snapshot = ReviewCommentSnapshot {
        pr_level_comments: Vec::new(),
        threads: Vec::new(),
    };

    // Act
    let loading = buffer_text(&render_review_comment_page(
        None,
        None,
        true,
        0,
        0,
        (80, 12),
    ));
    let error = buffer_text(&render_review_comment_page(
        None,
        Some("Forge unavailable"),
        false,
        0,
        0,
        (80, 12),
    ));
    let empty = buffer_text(&render_review_comment_page(
        Some(&empty_snapshot),
        None,
        false,
        0,
        0,
        (80, 12),
    ));
    let missing = buffer_text(&render_review_comment_page(
        None,
        None,
        false,
        0,
        0,
        (80, 12),
    ));

    // Assert
    assert!(loading.contains("Loading..."));
    assert!(loading.contains("Loading review comments..."));
    assert!(error.contains("Load failed"));
    assert!(error.contains("Forge unavailable"));
    assert!(empty.contains("No comments"));
    assert!(empty.contains("No review comments."));
    assert!(missing.contains("No review comments."));
}

#[test]
fn test_review_comment_view_max_scroll_offset_reflects_detail_overflow() {
    // Arrange
    let snapshot = comment_snapshot();
    let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();
    let markdown_render_cache = markdown::MarkdownRenderCache::default();

    // Act
    let scroll_offset = review_comment_view_max_scroll_offset(
        Some(&snapshot),
        None,
        false,
        SAMPLE_DIFF,
        ReviewCommentRenderCaches {
            diff_layout: &diff_layout_cache,
            markdown: &markdown_render_cache,
        },
        0,
        Rect::new(0, 0, 80, 10),
    );

    // Assert
    assert!(scroll_offset > 0);
}

#[test]
fn test_code_context_lines_include_and_highlight_attached_new_line() {
    // Arrange
    let thread = inline_thread(2);
    let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();

    // Act
    let lines = code_context_lines(&thread, SAMPLE_DIFF, &diff_layout_cache, 80);

    // Assert
    let attached_line = lines
        .iter()
        .find(|line| line.to_string().contains("println!"))
        .expect("attached line should be visible");
    assert!(attached_line.to_string().contains("+    println!"));
    assert!(
        attached_line
            .spans
            .iter()
            .all(|span| span.style.bg == Some(style::palette::surface_selection()))
    );
}

#[test]
fn test_code_context_lines_repeat_inline_derivation_with_one_shared_cache() {
    // Arrange
    let thread = inline_thread(2);
    let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();
    let diff = concat!(
        "diff --git a/src/unrelated.rs b/src/unrelated.rs\n",
        "@@ -1 +1 @@\n",
        "-unrelated old\n",
        "+unrelated new\n",
        "diff --git a/src/main.rs b/src/main.rs\n",
        "@@ -1,3 +1,4 @@\n",
        " fn main() {\n",
        "+    println!(\"hello\");\n",
        " }\n",
    );

    // Act
    let first_lines = code_context_lines(&thread, diff, &diff_layout_cache, 80);
    let repeated_lines = code_context_lines(&thread, diff, &diff_layout_cache, 80);

    // Assert
    assert_eq!(first_lines, repeated_lines);
    assert!(
        repeated_lines
            .iter()
            .all(|line| !line.to_string().contains("unrelated"))
    );
    assert!(
        repeated_lines
            .iter()
            .any(|line| line.to_string().contains("println!"))
    );
}

#[test]
fn test_code_context_lines_highlight_every_line_in_multiline_anchor_range() {
    // Arrange
    let mut thread = inline_thread(2);
    thread.start_line = Some(1);
    let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();

    // Act
    let lines = code_context_lines(&thread, SAMPLE_DIFF, &diff_layout_cache, 80);

    // Assert
    let start_line = lines
        .iter()
        .find(|line| line.to_string().contains("fn main"))
        .expect("attached range start should be visible");
    let end_line = lines
        .iter()
        .find(|line| line.to_string().contains("println!"))
        .expect("attached range end should be visible");
    let trailing_line = lines
        .iter()
        .find(|line| line.to_string().contains('}'))
        .expect("surrounding context should be visible");
    assert!(
        start_line
            .spans
            .iter()
            .all(|span| span.style.bg == Some(style::palette::surface_selection()))
    );
    assert!(
        end_line
            .spans
            .iter()
            .all(|span| span.style.bg == Some(style::palette::surface_selection()))
    );
    assert!(
        trailing_line
            .spans
            .iter()
            .all(|span| span.style.bg.is_none())
    );
}

#[test]
fn test_code_context_line_uses_diff_page_gutter_and_change_styles() {
    // Arrange
    let addition = DiffLine {
        content: "added",
        kind: DiffLineKind::Addition,
        new_line: Some(2),
        old_line: None,
    };
    let deletion = DiffLine {
        content: "removed",
        kind: DiffLineKind::Deletion,
        new_line: None,
        old_line: Some(2),
    };

    // Act
    let addition_line = code_context_line(&addition, false, 1, 80);
    let deletion_line = code_context_line(&deletion, false, 1, 80);

    // Assert
    assert_eq!(
        addition_line.spans[0].style.fg,
        Some(style::palette::text_subtle())
    );
    assert_eq!(
        addition_line.spans[1].style.bg,
        Some(style::palette::surface_success())
    );
    assert_eq!(
        deletion_line.spans[0].style.fg,
        Some(style::palette::text_subtle())
    );
    assert_eq!(
        deletion_line.spans[1].style.bg,
        Some(style::palette::surface_danger())
    );
}

#[test]
fn test_code_context_lines_explain_file_level_anchor_without_synthetic_code() {
    // Arrange
    let thread = file_thread();
    let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();

    // Act
    let lines = code_context_lines(&thread, SAMPLE_DIFF, &diff_layout_cache, 80);

    // Assert
    assert_eq!(
        lines.iter().map(Line::to_string).collect::<Vec<_>>(),
        vec!["This file-level comment is not attached to a code line."]
    );
    assert!(
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .all(|span| span.style.bg.is_none())
    );
}

#[test]
fn test_code_context_lines_explain_outdated_anchor_without_current_diff_context() {
    // Arrange
    let mut thread = inline_thread(2);
    thread.is_outdated = Some(true);
    let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();

    // Act
    let lines = code_context_lines(&thread, SAMPLE_DIFF, &diff_layout_cache, 80);

    // Assert
    assert_eq!(
        lines.iter().map(Line::to_string).collect::<Vec<_>>(),
        vec!["Original code context unavailable."]
    );
    assert!(
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .all(|span| span.style.bg.is_none())
    );
}

#[test]
fn test_code_context_lines_cover_old_side_and_missing_anchor_fallbacks() {
    // Arrange
    let mut old_thread = inline_thread(1);
    old_thread.anchor_side = ReviewCommentAnchorSide::Old;
    let mut missing_file_thread = inline_thread(2);
    missing_file_thread.path = "src/missing.rs".to_string();
    let outside_thread = inline_thread(99);
    let mut missing_anchor_thread = inline_thread(2);
    missing_anchor_thread.line = None;
    let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();

    // Act
    let old_lines = code_context_lines(&old_thread, SAMPLE_DIFF, &diff_layout_cache, 80);
    let missing_file_lines =
        code_context_lines(&missing_file_thread, SAMPLE_DIFF, &diff_layout_cache, 80);
    let outside_lines = code_context_lines(&outside_thread, SAMPLE_DIFF, &diff_layout_cache, 80);
    let missing_anchor_lines =
        code_context_lines(&missing_anchor_thread, SAMPLE_DIFF, &diff_layout_cache, 80);
    let file_side_matches = diff_line_matches_anchor(
        &DiffLine {
            content: "file",
            kind: DiffLineKind::Context,
            new_line: Some(1),
            old_line: Some(1),
        },
        ReviewCommentAnchorSide::File,
        (1, 1),
    );

    // Assert
    let old_anchor = old_lines
        .iter()
        .find(|line| line.to_string().contains("fn main"))
        .expect("old-side anchor should be visible");
    assert!(
        old_anchor
            .spans
            .iter()
            .all(|span| span.style.bg == Some(style::palette::surface_selection()))
    );
    assert_eq!(
        missing_file_lines[0].to_string(),
        "No current diff context is available for this file."
    );
    assert_eq!(
        outside_lines[0].to_string(),
        "The attached line or range is outside the current diff context."
    );
    assert_eq!(
        missing_anchor_lines[0].to_string(),
        "This comment has no attached line anchor."
    );
    assert!(!file_side_matches);
}

#[test]
fn test_comment_detail_lines_include_thread_metadata_body_and_code() {
    // Arrange
    let snapshot = ReviewCommentSnapshot {
        pr_level_comments: Vec::new(),
        threads: vec![inline_thread(2)],
    };
    let rows = review_comment_selection::grouped_review_comment_rows(&snapshot);
    let diff_layout_cache = crate::ui::page::diff::DiffLayoutCache::default();
    let markdown_render_cache = markdown::MarkdownRenderCache::default();

    // Act
    let lines = comment_detail_lines(
        Some(&rows),
        None,
        false,
        SAMPLE_DIFF,
        ReviewCommentRenderCaches {
            diff_layout: &diff_layout_cache,
            markdown: &markdown_render_cache,
        },
        0,
        80,
    );
    let text = lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let code_context_index = lines
        .iter()
        .position(|line| line.to_string() == "Code context")
        .expect("code context section should be visible");
    let conversation_index = lines
        .iter()
        .position(|line| line.to_string() == "Conversation")
        .expect("conversation section should be visible");

    // Assert
    assert!(text.contains("src/main.rs:2"));
    assert!(text.contains("Please explain this output."));
    assert!(text.contains("Code context"));
    assert!(text.contains("println!(\"review\")"));
    assert!(code_context_index < conversation_index);
}

#[test]
fn test_review_comment_item_count_includes_general_comments_and_threads() {
    // Arrange
    let snapshot = ReviewCommentSnapshot {
        pr_level_comments: vec![ReviewComment {
            author: "bob".to_string(),
            authored_by_current_user: false,
            body: "General note".to_string(),
        }],
        threads: vec![inline_thread(2), inline_thread(3)],
    };

    // Act
    let count = review_comment_item_count(Some(&snapshot));

    // Assert
    assert_eq!(count, 3);
}

#[test]
fn test_comment_list_items_map_grouped_rows_to_selectable_indexes() {
    // Arrange
    let mut resolved = inline_thread(3);
    resolved.id = "resolved".to_string();
    resolved.is_resolved = true;
    let mut unresolved = inline_thread(2);
    unresolved.id = "unresolved".to_string();
    let snapshot = ReviewCommentSnapshot {
        pr_level_comments: vec![ReviewComment {
            author: "bob".to_string(),
            authored_by_current_user: false,
            body: "Standalone note".to_string(),
        }],
        threads: vec![resolved, unresolved],
    };

    // Act
    let rows = review_comment_selection::grouped_review_comment_rows(&snapshot);
    let (items, selection_rows) = comment_list_items(&rows, &[]);

    // Assert
    assert_eq!(items.len(), 6);
    assert_eq!(selection_rows, vec![1, 3, 5]);
}

#[test]
fn test_review_comment_actionability_includes_outdated_unresolved_rows() {
    // Arrange
    let mut current = inline_thread(2);
    current.id = "current".to_string();
    let mut resolved = inline_thread(3);
    resolved.id = "resolved".to_string();
    resolved.is_resolved = true;
    let mut outdated = inline_thread(4);
    outdated.id = "outdated".to_string();
    outdated.is_outdated = Some(true);
    let snapshot = ReviewCommentSnapshot {
        pr_level_comments: vec![ReviewComment {
            author: "bob".to_string(),
            authored_by_current_user: false,
            body: "General note".to_string(),
        }],
        threads: vec![current, resolved, outdated],
    };
    let rows = review_comment_selection::grouped_review_comment_rows(&snapshot);

    // Act
    let current_is_actionable = review_comment_selected_is_actionable(&rows, 0);
    let outdated_is_actionable = review_comment_selected_is_actionable(&rows, 1);
    let resolved_is_actionable = review_comment_selected_is_actionable(&rows, 2);
    let general_is_actionable = review_comment_selected_is_actionable(&rows, 3);
    let missing_is_actionable = review_comment_selected_is_actionable(&rows, 99);
    let current_thread_id = review_comment_selection::selected_thread_id(&snapshot, 0);
    let general_thread_id = review_comment_selection::selected_thread_id(&snapshot, 3);

    // Assert
    assert!(!general_is_actionable);
    assert!(current_is_actionable);
    assert!(!resolved_is_actionable);
    assert!(outdated_is_actionable);
    assert!(!missing_is_actionable);
    assert_eq!(current_thread_id, Some("current"));
    assert_eq!(general_thread_id, None);
    assert!(!review_comment_selected_is_actionable(&[], 0));
}

#[test]
fn test_review_comment_actionability_is_false_without_general_or_current_threads() {
    // Arrange
    let mut resolved = inline_thread(2);
    resolved.is_resolved = true;
    let snapshot = ReviewCommentSnapshot {
        pr_level_comments: vec![ReviewComment {
            author: "bob".to_string(),
            authored_by_current_user: false,
            body: "Standalone note".to_string(),
        }],
        threads: vec![resolved],
    };
    let rows = review_comment_selection::grouped_review_comment_rows(&snapshot);

    // Act, Assert
    assert!(!review_comment_selected_is_actionable(&rows, 0));
    assert!(!review_comment_selected_is_actionable(&rows, 1));
}
