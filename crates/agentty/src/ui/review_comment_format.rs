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

/// Renders the `path:line · side · N comments · resolved/unresolved` header
/// shared by review-detail and diff comment panels.
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

/// Returns the file-and-line anchor for one review thread.
fn thread_anchor(thread: &ReviewCommentThread) -> String {
    match thread.line {
        Some(line) => format!("{}:{line}", thread.path),
        None => thread.path.clone(),
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
    fn test_thread_header_line_includes_outdated_resolution_metadata() {
        // Arrange
        let thread = ReviewCommentThread {
            anchor_side: ReviewCommentAnchorSide::Old,
            comments: vec![ReviewComment {
                author: "alice".to_string(),
                body: "Please check this.".to_string(),
            }],
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
}
