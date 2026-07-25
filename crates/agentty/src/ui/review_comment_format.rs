use ag_forge::{ReviewComment, ReviewCommentAnchorSide, ReviewCommentThread};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::{markdown, style};

/// Appends comment author rows and two-space-indented markdown bodies.
pub(crate) fn append_comment_bodies(
    lines: &mut Vec<Line<'static>>,
    comments: &[ReviewComment],
    markdown_render_cache: &markdown::MarkdownRenderCache,
    width: usize,
) {
    for (comment_index, comment) in comments.iter().enumerate() {
        if comment_index > 0 {
            lines.push(Line::default());
        }

        append_comment_body(lines, comment, markdown_render_cache, width);
    }
}

/// Renders the anchor, side, comment count, resolution, and optional outdated
/// metadata shared by review-detail and diff comment panels.
pub(crate) fn thread_header_line(
    thread: &ReviewCommentThread,
    anchor_style: Style,
) -> Line<'static> {
    let anchor = thread_anchor(thread);
    let side_tag = anchor_side_tag(thread.anchor_side);
    let comment_count = thread.comments.len();
    let resolution_tag = if thread.is_resolved {
        "resolved"
    } else {
        "unresolved"
    };
    let outdated_tag = if thread.is_outdated == Some(true) {
        "  ·  outdated"
    } else {
        ""
    };

    Line::from(vec![
        Span::styled(anchor, anchor_style),
        Span::styled(
            format!(
                "  ·  {side_tag}  ·  {comment_count} comments  ·  {resolution_tag}{outdated_tag}"
            ),
            Style::default().fg(style::palette::text_muted()),
        ),
    ])
}

/// Returns the file-and-line or file-and-range anchor for one review thread.
pub(crate) fn thread_anchor(thread: &ReviewCommentThread) -> String {
    match thread_anchor_line_range(thread) {
        Some((start_line, end_line)) if start_line != end_line => {
            format!("{}:{start_line}-{end_line}", thread.path)
        }
        Some((_, end_line)) => format!("{}:{end_line}", thread.path),
        None => thread.path.clone(),
    }
}

/// Returns the normalized inclusive line range attached to one inline thread.
pub(crate) fn thread_anchor_line_range(thread: &ReviewCommentThread) -> Option<(u32, u32)> {
    if thread.anchor_side == ReviewCommentAnchorSide::File {
        return None;
    }
    let end_line = thread.line?;
    let start_line = thread.start_line.unwrap_or(end_line);

    Some((start_line.min(end_line), start_line.max(end_line)))
}

/// Appends one comment's author header followed by the markdown-rendered body.
fn append_comment_body(
    lines: &mut Vec<Line<'static>>,
    comment: &ReviewComment,
    markdown_render_cache: &markdown::MarkdownRenderCache,
    width: usize,
) {
    lines.push(Line::from(Span::styled(
        comment.author.clone(),
        Style::default()
            .fg(style::palette::text())
            .add_modifier(Modifier::BOLD),
    )));

    let body_width = width.saturating_sub(2).max(1);
    let rendered = markdown_render_cache.render(&comment.body, body_width);
    for rendered_line in rendered.iter() {
        let mut spans = Vec::with_capacity(rendered_line.spans.len() + 1);
        spans.push(Span::raw("  "));
        spans.extend(rendered_line.spans.iter().cloned());
        lines.push(Line::from(spans));
    }
}

/// Returns the display tag for one review thread anchor side.
fn anchor_side_tag(anchor_side: ReviewCommentAnchorSide) -> &'static str {
    match anchor_side {
        ReviewCommentAnchorSide::File => "file",
        ReviewCommentAnchorSide::New => "new",
        ReviewCommentAnchorSide::Old => "old",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thread_header_line_includes_resolution_and_outdated_metadata() {
        // Arrange
        let thread = ReviewCommentThread {
            anchor_side: ReviewCommentAnchorSide::Old,
            comments: vec![ReviewComment {
                author: "alice".to_string(),
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
    fn test_append_comment_bodies_indents_markdown_under_author() {
        // Arrange
        let mut lines = Vec::new();
        let comments = vec![ReviewComment {
            author: "alice".to_string(),
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
    fn test_append_comment_bodies_separates_multiple_comments() {
        // Arrange
        let mut lines = Vec::new();
        let comments = vec![
            ReviewComment {
                author: "alice".to_string(),
                body: "First".to_string(),
            },
            ReviewComment {
                author: "bob".to_string(),
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
}
