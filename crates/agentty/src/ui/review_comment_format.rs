use ag_forge::{ReviewComment, ReviewCommentAnchorSide, ReviewCommentThread};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::{markdown, style};

/// Appends comment author rows and two-space-indented Markdown bodies after
/// normalizing embedded HTML.
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
    let addressed_tag = if thread.is_addressed_by_agentty() {
        "  ·  addressed"
    } else {
        ""
    };

    Line::from(vec![
        Span::styled(anchor, anchor_style),
        Span::styled(
            format!(
                "  ·  {side_tag}  ·  {comment_count} comments  ·  \
                 {resolution_tag}{outdated_tag}{addressed_tag}"
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

/// Appends one comment's author header followed by its rendered body.
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
    let rendered = markdown_render_cache.render_html(&comment.body, body_width);
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
#[path = "review_comment_format_test.rs"]
mod tests;
