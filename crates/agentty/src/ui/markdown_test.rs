use std::sync::Arc;

use ratatui::style::Style;

use super::{MarkdownRenderCache, render_markdown};
use crate::ui::{prompt_block, style};

#[test]
fn test_render_markdown_parses_prompt_continuation_line_markdown() {
    // Arrange
    let input = " › first line\n**bold**\n\nassistant";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines[2].to_string().trim_end(), "   bold");
    assert_eq!(
        lines[2].spans[0].style,
        prompt_block::user_prompt_content_style()
    );
    assert!(lines[2].spans.iter().any(|span| {
        span.content.as_ref() == "bold"
            && span
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
    }));
    assert_eq!(lines[4].to_string(), "");
    assert_eq!(lines[5].to_string(), "assistant");
}

#[test]
fn test_render_markdown_wraps_fenced_code_on_word_boundaries() {
    // Arrange
    let input = "```text\nformatted blocks in user messages without words breaking\n```";

    // Act
    let lines = render_markdown(input, 32);
    let rendered_lines = lines.iter().map(ToString::to_string).collect::<Vec<_>>();

    // Assert
    assert_eq!(rendered_lines[0], "formatted blocks in user ");
    assert_eq!(rendered_lines[1], "messages without words breaking");
    assert!(!rendered_lines.iter().any(|line| line.ends_with("message")));
    assert!(!rendered_lines.iter().any(|line| line.starts_with("s ")));
    let code_block_style = Style::default()
        .fg(style::palette::text_muted())
        .bg(style::palette::surface_overlay());
    assert!(lines.iter().all(|line| {
        line.spans
            .first()
            .is_some_and(|span| span.style == code_block_style)
    }));
}

#[test]
fn test_markdown_render_cache_reuses_prompt_block_lines() {
    // Arrange
    let cache = MarkdownRenderCache::default();
    let input = " › **bold** prompt";

    // Act
    let first_lines = cache.render(input, 80);
    let cached_lines = cache.render(input, 80);

    // Assert
    assert!(Arc::ptr_eq(&first_lines, &cached_lines));
    assert!(first_lines.iter().any(|line| {
        line.spans.iter().any(|span| {
            span.content.as_ref() == "bold"
                && span
                    .style
                    .add_modifier
                    .contains(ratatui::style::Modifier::BOLD)
        })
    }));
}

#[test]
fn test_markdown_render_cache_keeps_html_entries_separate_and_invalidates_them() {
    // Arrange
    let cache = MarkdownRenderCache::default();
    let input = "Use <strong>shared</strong> rendering.";
    let initial_version = cache.version();

    // Act
    let markdown_lines = cache.render(input, 80);
    let html_lines = cache.render_html(input, 80);
    let cached_html_lines = cache.render_html(input, 80);
    cache.bump_version();
    let refreshed_markdown_lines = cache.render(input, 80);
    let refreshed_html_lines = cache.render_html(input, 80);

    // Assert
    assert_eq!(markdown_lines[0].to_string(), input);
    assert_eq!(html_lines[0].to_string(), "Use shared rendering.");
    assert!(!Arc::ptr_eq(&markdown_lines, &html_lines));
    assert!(Arc::ptr_eq(&html_lines, &cached_html_lines));
    assert!(!Arc::ptr_eq(&markdown_lines, &refreshed_markdown_lines));
    assert!(!Arc::ptr_eq(&html_lines, &refreshed_html_lines));
    assert_eq!(cache.version(), initial_version + 1);
}

#[test]
fn test_render_markdown_keeps_consecutive_prompt_blocks_separate() {
    // Arrange
    let input = " › first prompt\n › second prompt";

    // Act
    let lines = render_markdown(input, 80);
    let rendered_lines = lines
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        rendered_lines
            .iter()
            .filter(|line| line.as_str() == " › first prompt")
            .count(),
        1
    );
    assert_eq!(
        rendered_lines
            .iter()
            .filter(|line| line.as_str() == " › second prompt")
            .count(),
        1
    );
    assert!(
        !rendered_lines
            .iter()
            .any(|line| line == "   › second prompt")
    );
}
