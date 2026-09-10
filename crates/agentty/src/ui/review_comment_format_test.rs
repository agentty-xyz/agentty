use ag_forge::{ReviewComment, ReviewCommentAnchorSide, ReviewCommentThread};
use ratatui::style::Style;
use ratatui::text::Line;

use super::{append_comment_bodies, thread_anchor_line_range, thread_header_line};
use crate::ui::markdown;

#[test]
fn test_thread_header_line_includes_resolution_and_outdated_metadata() {
    // Arrange
    let thread = ReviewCommentThread {
        anchor_side: ReviewCommentAnchorSide::Old,
        comments: vec![ReviewComment {
            author: "alice".to_string(),
            authored_by_current_user: false,
            body: "Please check this.".to_string(),
        }],
        id: "thread-id".to_string(),
        is_outdated: Some(true),
        is_resolved: true,
        line: Some(12),
        path: "src/lib.rs".to_string(),
        start_line: None,
    };

    // Act
    let line = thread_header_line(&thread, Style::default());

    // Assert
    assert_eq!(
        line.to_string(),
        "src/lib.rs:12  ·  old  ·  1 comments  ·  resolved  ·  outdated"
    );
}

#[test]
fn test_thread_header_line_includes_multiline_anchor_range() {
    // Arrange
    let thread = ReviewCommentThread {
        anchor_side: ReviewCommentAnchorSide::New,
        comments: vec![ReviewComment {
            author: "alice".to_string(),
            authored_by_current_user: false,
            body: "Please check these lines.".to_string(),
        }],
        id: "thread-id".to_string(),
        is_outdated: Some(false),
        is_resolved: false,
        line: Some(12),
        path: "src/lib.rs".to_string(),
        start_line: Some(10),
    };

    // Act
    let line = thread_header_line(&thread, Style::default());

    // Assert
    assert_eq!(
        line.to_string(),
        "src/lib.rs:10-12  ·  new  ·  1 comments  ·  unresolved"
    );
}

#[test]
fn test_thread_header_line_marks_unresolved_agentty_reply_as_addressed() {
    // Arrange
    let thread = ReviewCommentThread {
        anchor_side: ReviewCommentAnchorSide::New,
        comments: vec![ReviewComment {
            author: "agentty".to_string(),
            authored_by_current_user: true,
            body: concat!(
                "No change needed.\n\n",
                "<!-- agentty review resolution:",
                "123e4567-e89b-12d3-a456-426614174000 -->",
            )
            .to_string(),
        }],
        id: "thread-id".to_string(),
        is_outdated: Some(false),
        is_resolved: false,
        line: Some(12),
        path: "src/lib.rs".to_string(),
        start_line: None,
    };

    // Act
    let line = thread_header_line(&thread, Style::default());

    // Assert
    assert_eq!(
        line.to_string(),
        "src/lib.rs:12  ·  new  ·  1 comments  ·  unresolved  ·  addressed"
    );
}

#[test]
fn test_append_comment_bodies_indents_markdown_under_author() {
    // Arrange
    let mut lines = Vec::new();
    let comments = vec![ReviewComment {
        author: "alice".to_string(),
        authored_by_current_user: false,
        body: "**Looks** good.".to_string(),
    }];
    let markdown_render_cache = markdown::MarkdownRenderCache::default();

    // Act
    append_comment_bodies(&mut lines, &comments, &markdown_render_cache, 40);

    // Assert
    let text = lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("alice"));
    assert!(text.contains("  Looks good."));
}

#[test]
fn test_append_comment_bodies_renders_embedded_html() {
    // Arrange
    let mut lines = Vec::new();
    let comments = vec![ReviewComment {
        author: "alice".to_string(),
        authored_by_current_user: false,
        body: concat!(
            "<!-- hidden reviewer note -->",
            "<p><strong>Explain</strong> this output.<br>",
            "Use <code>stdout</code>.</p>",
        )
        .to_string(),
    }];
    let markdown_render_cache = markdown::MarkdownRenderCache::default();

    // Act
    append_comment_bodies(&mut lines, &comments, &markdown_render_cache, 40);
    let text = lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("  Explain this output."));
    assert!(text.contains("  Use stdout."));
    assert!(!text.contains("hidden reviewer note"));
    assert!(!text.contains("<strong>"));
    assert!(!text.contains("<br>"));
}

#[test]
fn test_append_comment_bodies_separates_multiple_comments() {
    // Arrange
    let mut lines = Vec::new();
    let comments = vec![
        ReviewComment {
            author: "alice".to_string(),
            authored_by_current_user: false,
            body: "First".to_string(),
        },
        ReviewComment {
            author: "bob".to_string(),
            authored_by_current_user: false,
            body: "Second".to_string(),
        },
    ];
    let markdown_render_cache = markdown::MarkdownRenderCache::default();

    // Act
    append_comment_bodies(&mut lines, &comments, &markdown_render_cache, 40);

    // Assert
    assert!(lines.iter().any(|line| line.spans.is_empty()));
    assert!(lines.iter().any(|line| line.to_string() == "alice"));
    assert!(lines.iter().any(|line| line.to_string() == "bob"));
}

#[test]
fn test_thread_header_line_shows_file_level_anchor() {
    // Arrange
    let thread = ReviewCommentThread {
        anchor_side: ReviewCommentAnchorSide::File,
        comments: Vec::new(),
        id: "thread-id".to_string(),
        is_outdated: None,
        is_resolved: false,
        line: None,
        path: "src/lib.rs".to_string(),
        start_line: None,
    };

    // Act
    let line = thread_header_line(&thread, Style::default());

    // Assert
    assert_eq!(
        line.to_string(),
        "src/lib.rs  ·  file  ·  0 comments  ·  unresolved"
    );
    assert_eq!(thread_anchor_line_range(&thread), None);
}
